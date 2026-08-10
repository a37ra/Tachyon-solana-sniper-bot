//! Create-instruction signal source via `logsSubscribe`.
//!
//! Primary new-pool detection: only `Instruction: Create` / Initialize* log markers
//! (see `logs_parser`). Pump.fun mint+curve are decoded from `Program data:` when
//! present; other DEXes resolve accounts via one `getTransaction` on confirmed creates.
//!
//! `SeenPools` is dedup-only (duplicate WS deliveries), not "is this account new".

use crate::signal::logs_parser;
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
    // Dedup only — create ix cannot fire twice for the same pool; warmup not needed.
    let warmup = std::env::var("SOLBOT_SIGNAL_WARMUP_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
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
                { "commitment": "processed" }
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
                            if !ps.instruction_type.is_create_event() {
                                continue;
                            }

                            // Fast path: pump Create with Program data fully decoded.
                            if ps.resolved_from_log_data {
                                if let (Some(mint), pool) = (ps.mint, ps.pool_address) {
                                    if let Some(signal) = finalize_signal(
                                        mint.to_string(),
                                        pool.to_string(),
                                        ps.dex.program_id().to_string(),
                                        ps.slot,
                                        seen,
                                    ) {
                                        info!(
                                            mint = %signal.mint,
                                            pool = %signal.pool_address,
                                            dex = %signal.dex_program,
                                            "create-ix signal (log-data)"
                                        );
                                        if tx.send(signal).await.is_err() {
                                            return Ok(());
                                        }
                                    }
                                }
                                continue;
                            }

                            let Some(sig_str) = ps.signature.as_ref() else { continue };
                            match resolve_from_tx(rpc.as_ref(), sig_str, ps.slot, program_ids, seen).await {
                                Ok(Some(signal)) => {
                                    info!(
                                        mint = %signal.mint,
                                        pool = %signal.pool_address,
                                        dex = %signal.dex_program,
                                        "create-ix signal (getTransaction)"
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
    heuristic_from_keys(&tx.message.account_keys, program_ids, slot, seen)
}

fn extract_from_versioned(
    vtx: &solana_transaction::versioned::VersionedTransaction,
    program_ids: &[Pubkey],
    slot: u64,
    seen: &SeenPools,
) -> Option<Signal> {
    use solana_message::VersionedMessage;
    let keys = vtx.message.static_account_keys();

    let ixs: Vec<_> = match &vtx.message {
        VersionedMessage::Legacy(m) => m
            .instructions
            .iter()
            .map(|ix| (ix.program_id_index, ix.accounts.as_slice(), ix.data.as_slice()))
            .collect(),
        VersionedMessage::V0(m) => m
            .instructions
            .iter()
            .map(|ix| (ix.program_id_index, ix.accounts.as_slice(), ix.data.as_slice()))
            .collect(),
    };

    for (prog_idx, acc_idxs, data) in ixs {
        let prog = keys.get(prog_idx as usize)?;
        if !program_ids.iter().any(|p| p == prog) {
            continue;
        }
        // Only treat as create when instruction data looks like init (not every ix).
        if !looks_like_create_ix(prog, data) {
            continue;
        }
        if let Some((mint, pool)) = resolve_accounts_for_program(prog, keys, acc_idxs, data) {
            if let Some(s) = finalize_signal(mint, pool, prog.to_string(), slot, seen) {
                return Some(s);
            }
        }
    }

    None
}

/// Gate getTransaction account extraction so swaps in the same tx as a create log
/// (or mis-tagged logs) do not invent random mint/pool pairs.
fn looks_like_create_ix(prog: &Pubkey, data: &[u8]) -> bool {
    let p = prog.to_string();
    if data.is_empty() {
        return false;
    }
    if p == "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" {
        // Anchor: 8-byte disc. Create/create_v2 — accept any non-empty; log already filtered.
        return data.len() >= 8;
    }
    if p == "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" {
        // Raydium AMM v4: initialize=0, initialize2=1
        return data[0] == 0 || data[0] == 1;
    }
    // Other Anchor programs: 8-byte disc present; trust upstream log create filter.
    data.len() >= 8
}

fn resolve_accounts_for_program(
    prog: &Pubkey,
    keys: &[Pubkey],
    acc_idxs: &[u8],
    data: &[u8],
) -> Option<(String, String)> {
    let p = prog.to_string();
    let get = |i: usize| -> Option<String> {
        let idx = *acc_idxs.get(i)? as usize;
        Some(keys.get(idx)?.to_string())
    };

    if acc_idxs.len() < 3 {
        return None;
    }

    let _ = data;

    if p == "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" {
        // Pump create accounts: mint@0, bonding_curve@2 (common layout)
        let mint = get(0)?;
        let pool = get(2).or_else(|| get(3))?;
        return Some((mint, pool));
    }
    if p == "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" {
        let pool = get(4).or_else(|| get(1))?;
        let mint = get(8).or_else(|| get(9)).or_else(|| get(6))?;
        return Some((mint, pool));
    }
    if p == "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" {
        let pool = get(3).or_else(|| get(0))?;
        let mint = get(4).or_else(|| get(5))?;
        return Some((mint, pool));
    }
    if p == "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" {
        let pool = get(2).or_else(|| get(3))?;
        let mint = get(3).or_else(|| get(4))?;
        return Some((mint, pool));
    }
    if p == "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"
        || p == "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG"
    {
        let pool = get(0).or_else(|| get(1))?;
        let mint = get(2).or_else(|| get(3))?;
        return Some((mint, pool));
    }

    let mut mint = None;
    let mut pool = None;
    for i in 0..acc_idxs.len().min(12) {
        let s = get(i)?;
        if s == SYSTEM_PROGRAM || s == NATIVE_MINT || s == p {
            continue;
        }
        if mint.is_none() {
            mint = Some(s);
        } else if pool.is_none() {
            pool = Some(s);
            break;
        }
    }
    Some((mint?, pool?))
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
        if is_plausible_spl_mint(k) {
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
    // Known blue-chips must never pass even if somehow present in a create tx.
    const BLOCKED: &[&str] = &[
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", // USDC
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", // USDT
        "So11111111111111111111111111111111111111112",  // wSOL
        "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So",   // mSOL
        "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",   // JUP
        "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263", // BONK
    ];
    if BLOCKED.contains(&mint.as_str()) {
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
    use crate::signal::logs_parser::{parse_log_lines, InstructionType};

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
    fn finalize_rejects_usdc() {
        let seen = SeenPools::with_warmup_secs(0);
        assert!(finalize_signal(
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
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

    #[test]
    fn swap_logs_do_not_finalize_via_create_filter() {
        let pump = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let logs = [
            format!("Program {pump} invoke [1]"),
            "Program log: Instruction: Buy".into(),
        ];
        let parsed = parse_log_lines(&logs, 1, "x");
        assert!(
            parsed
                .iter()
                .filter(|p| p.instruction_type.is_create_event())
                .count()
                == 0
        );
    }

    #[test]
    fn create_log_resolved_path_matches_finalize() {
        use base64::Engine;
        let mint = Pubkey::new_unique();
        let curve = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let mut body = Vec::new();
        for s in ["N", "S", "U"] {
            body.extend_from_slice(&(s.len() as u32).to_le_bytes());
            body.extend_from_slice(s.as_bytes());
        }
        body.extend_from_slice(mint.as_ref());
        body.extend_from_slice(curve.as_ref());
        body.extend_from_slice(user.as_ref());
        let mut full = vec![1u8; 8];
        full.extend_from_slice(&body);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&full);
        let pump = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let logs = [
            format!("Program {pump} invoke [1]"),
            "Program log: Instruction: Create".into(),
            format!("Program data: {b64}"),
        ];
        let parsed = parse_log_lines(&logs, 42, "sig");
        let c = parsed
            .iter()
            .find(|p| p.instruction_type == InstructionType::InitializePool)
            .expect("create");
        assert!(c.resolved_from_log_data);
        let seen = SeenPools::with_warmup_secs(0);
        let s = finalize_signal(
            c.mint.unwrap().to_string(),
            c.pool_address.to_string(),
            pump.into(),
            42,
            &seen,
        )
        .unwrap();
        assert_eq!(s.mint, mint.to_string());
        assert_eq!(s.pool_address, curve.to_string());
    }

    #[test]
    fn raydium_amm_create_disc_gate() {
        let pid = Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap();
        assert!(looks_like_create_ix(&pid, &[1, 0, 0]));
        assert!(looks_like_create_ix(&pid, &[0]));
        assert!(!looks_like_create_ix(&pid, &[9])); // swap base-in
    }
}
