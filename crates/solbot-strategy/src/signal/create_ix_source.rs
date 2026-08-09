//! Create-instruction signal source via `logsSubscribe` + `getTransaction`.
//!
//! Emits Signal only for InitializePool / InitBondingCurve / create-style logs,
//! then resolves mint/pool from the confirmed transaction account keys.

use crate::signal::logs_parser::{self, InstructionType};
use crate::signal::pool_signal::{is_plausible_spl_mint, SeenPools, NATIVE_MINT, SYSTEM_PROGRAM};
use anyhow::{anyhow, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Signature;
use solana_transaction_status::UiTransactionEncoding;
use solbot_common::types::Signal;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, Duration};
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{debug, error, info, warn};

/// Subscribe to create-pool / init-curve signals for the given program IDs.
pub async fn subscribe_create_ix_signals(
    program_ids: Vec<Pubkey>,
    rpc: Arc<RpcClient>,
) -> Result<mpsc::Receiver<Signal>> {
    let (tx, rx) = mpsc::channel(256);
    let warmup = std::env::var("SOLBOT_SIGNAL_WARMUP_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let seen = SeenPools::with_warmup_secs(warmup);

    let api_key = std::env::var("HELIUS_API_KEY")
        .or_else(|_| std::env::var("GEYSER_X_TOKEN"))
        .unwrap_or_else(|_| "YOUR_API_KEY".to_string());
    let ws_url = std::env::var("HELIUS_WS_URL")
        .unwrap_or_else(|_| format!("wss://mainnet.helius-rpc.com/?api-key={}", api_key));

    tokio::spawn(async move {
        let mut attempts = 0u32;
        loop {
            match run_logs_loop(&ws_url, &program_ids, rpc.clone(), &tx, &seen).await {
                Ok(()) => return,
                Err(e) => {
                    attempts = attempts.saturating_add(1);
                    let backoff = Duration::from_secs((1u64 << attempts.min(5)).min(60));
                    error!(error = %e, attempt = attempts, ?backoff, "create-ix WS reconnect");
                    sleep(backoff).await;
                }
            }
        }
    });

    Ok(rx)
}

async fn run_logs_loop(
    ws_url: &str,
    program_ids: &[Pubkey],
    rpc: Arc<RpcClient>,
    tx: &mpsc::Sender<Signal>,
    seen: &SeenPools,
) -> Result<()> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await?;
    let (mut write, mut read) = ws_stream.split();

    for (i, pid) in program_ids.iter().enumerate() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": i + 1,
            "method": "logsSubscribe",
            "params": [
                { "mentions": [pid.to_string()] },
                { "commitment": "confirmed" }
            ]
        });
        write.send(Message::Text(msg.to_string())).await?;
        info!(program = %pid, "create-ix logsSubscribe OK");
    }

    let mut ping = interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = ping.tick() => {
                let _ = write.send(Message::Ping(vec![])).await;
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let parsed = match logs_parser::parse_logs_message(&text) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        for ps in parsed {
                            if !matches!(
                                ps.instruction_type,
                                InstructionType::InitializePool
                                    | InstructionType::InitBondingCurve
                                    | InstructionType::Migrate
                            ) {
                                continue;
                            }
                            let Some(sig_str) = ps.signature.as_ref() else { continue };
                            match resolve_from_tx(rpc.as_ref(), sig_str, ps.slot, program_ids, seen).await {
                                Ok(Some(signal)) => {
                                    info!(
                                        mint = %signal.mint,
                                        pool = %signal.pool_address,
                                        dex = %signal.dex_program,
                                        "create-ix signal"
                                    );
                                    if tx.send(signal).await.is_err() {
                                        return Ok(());
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => debug!(error = %e, "create-ix resolve"),
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        warn!("create-ix WS closed");
                        anyhow::bail!("websocket closed");
                    }
                    Some(Err(e)) => return Err(e.into()),
                    _ => {}
                }
            }
        }
    }
}

async fn resolve_from_tx(
    rpc: &RpcClient,
    signature: &str,
    slot: u64,
    program_ids: &[Pubkey],
    seen: &SeenPools,
) -> Result<Option<Signal>> {
    let sig = Signature::from_str(signature)?;
    let conf = rpc
        .get_transaction(&sig, UiTransactionEncoding::Base64)
        .await
        .map_err(|e| anyhow!("getTransaction: {e}"))?;

    let encoded = conf.transaction.transaction;
    let bytes = match encoded {
        solana_transaction_status::EncodedTransaction::Binary(b64, _) => {
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| anyhow!("b64: {e}"))?
        }
        solana_transaction_status::EncodedTransaction::LegacyBinary(b64) => {
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| anyhow!("b64: {e}"))?
        }
        _ => return Ok(None),
    };

    // Try legacy Transaction first, then VersionedTransaction message keys.
    if let Ok(tx) = bincode::deserialize::<solana_transaction::Transaction>(&bytes) {
        if let Some(s) = extract_from_legacy(&tx, program_ids, slot, seen) {
            return Ok(Some(s));
        }
    }

    if let Ok(vtx) = bincode::deserialize::<solana_transaction::versioned::VersionedTransaction>(&bytes)
    {
        if let Some(s) = extract_from_versioned(&vtx, program_ids, slot, seen) {
            return Ok(Some(s));
        }
    }

    Ok(None)
}

fn extract_from_legacy(
    tx: &solana_transaction::Transaction,
    program_ids: &[Pubkey],
    slot: u64,
    seen: &SeenPools,
) -> Option<Signal> {
    use crate::signal::instruction_parser::{parse_transaction_instructions, ParsedInstruction};
    for inst in parse_transaction_instructions(tx, program_ids) {
        if let ParsedInstruction::InitializePool {
            mint,
            pool_address,
            dex_program,
        } = inst
        {
            return finalize_signal(mint, pool_address, dex_program, slot, seen);
        }
    }
    // Heuristic: first non-system program ix → pool = first writable non-signer after program
    heuristic_from_keys(&tx.message.account_keys, program_ids, slot, seen)
}

fn extract_from_versioned(
    vtx: &solana_transaction::versioned::VersionedTransaction,
    program_ids: &[Pubkey],
    slot: u64,
    seen: &SeenPools,
) -> Option<Signal> {
    let keys = vtx.message.static_account_keys();
    heuristic_from_keys(keys, program_ids, slot, seen)
}

fn heuristic_from_keys(
    keys: &[Pubkey],
    program_ids: &[Pubkey],
    slot: u64,
    seen: &SeenPools,
) -> Option<Signal> {
    let mut dex = None;
    for k in keys {
        if program_ids.iter().any(|p| p == k) {
            dex = Some(k.to_string());
            break;
        }
    }
    let dex_program = dex?;

    // Prefer a key that looks like a mint (not system/native) and a separate pool key.
    let mut mint = None;
    let mut pool = None;
    for k in keys {
        let s = k.to_string();
        if s == SYSTEM_PROGRAM || s == NATIVE_MINT {
            continue;
        }
        if program_ids.iter().any(|p| p == k) {
            continue;
        }
        if is_plausible_spl_mint(k) && mint.is_none() {
            // Prefer addresses ending with "pump" when present (pump.fun mints).
            if s.ends_with("pump") {
                mint = Some(s.clone());
            } else if mint.is_none() {
                mint = Some(s.clone());
            }
        }
        if pool.is_none() && Some(s.as_str()) != mint.as_deref() {
            pool = Some(s);
        }
    }
    finalize_signal(mint?, pool?, dex_program, slot, seen)
}

fn finalize_signal(
    mint: String,
    pool_address: String,
    dex_program: String,
    slot: u64,
    seen: &SeenPools,
) -> Option<Signal> {
    let mint_pk = Pubkey::from_str(&mint).ok()?;
    if !is_plausible_spl_mint(&mint_pk) {
        return None;
    }
    if !seen.try_claim(&pool_address) {
        return None;
    }
    Some(Signal {
        mint,
        pool_address,
        dex_program,
        signal_source: "helius_logs_create_ix".into(),
        slot,
        received_at: chrono::Utc::now().timestamp_millis() as u64,
        raw_pool_account_data: None,
        raw_pool_account_owner: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_rejects_system_mint() {
        let seen = SeenPools::with_warmup_secs(0);
        assert!(finalize_signal(
            SYSTEM_PROGRAM.into(),
            Pubkey::new_unique().to_string(),
            "prog".into(),
            1,
            &seen
        )
        .is_none());
    }

    #[test]
    fn finalize_accepts_unique_mint() {
        let seen = SeenPools::with_warmup_secs(0);
        let mint = Pubkey::new_unique().to_string();
        let pool = Pubkey::new_unique().to_string();
        let s = finalize_signal(mint.clone(), pool, "prog".into(), 1, &seen).unwrap();
        assert_eq!(s.mint, mint);
        assert_eq!(s.signal_source, "helius_logs_create_ix");
    }
}
