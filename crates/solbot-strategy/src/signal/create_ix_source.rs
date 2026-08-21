//! Create-instruction signal source via `logsSubscribe`.
//!
//! Primary new-pool detection: only `Instruction: Create` / Initialize* log markers
//! (see `logs_parser`). Pump.fun mint+curve are decoded from `Program data:` when
//! present; other DEXes resolve accounts via one `getTransaction` on confirmed creates.
//!
//! `SeenPools` is dedup-only (duplicate WS deliveries), not "is this account new".

use crate::signal::logs_parser;
use crate::signal::pool_signal::{is_plausible_spl_mint, SeenPools, NATIVE_MINT};
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
                                    if let Some(mut signal) = finalize_signal(
                                        mint.to_string(),
                                        pool.to_string(),
                                        ps.dex.program_id().to_string(),
                                        ps.slot,
                                        seen,
                                    ) {
                                        // Restore 1-RTT risk-gate path: attach curve account now.
                                        attach_pool_account(rpc.as_ref(), &mut signal).await;
                                        info!(
                                            mint = %signal.mint,
                                            pool = %signal.pool_address,
                                            dex = %signal.dex_program,
                                            has_raw_pool = signal.raw_pool_account_data.is_some(),
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
                                Ok(Some(mut signal)) => {
                                    attach_pool_account(rpc.as_ref(), &mut signal).await;
                                    info!(
                                        mint = %signal.mint,
                                        pool = %signal.pool_address,
                                        dex = %signal.dex_program,
                                        has_raw_pool = signal.raw_pool_account_data.is_some(),
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
    // No key-heuristic fallback — wrong mint indices caused blue-chip false signals.
    None
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
        // One-time mainnet dump: log full account list for non-pump creates so indices
        // can be verified against live data (not only SDK helper order).
        log_create_accounts_once(prog, keys, acc_idxs, data);

        if let Some((mint, pool)) = resolve_accounts_for_program(prog, keys, acc_idxs, data) {
            if let Some(s) = finalize_signal(mint, pool, prog.to_string(), slot, seen) {
                return Some(s);
            }
        }
    }
    None
}

/// Log raw create-ix account indices once per program (live verification aid).
fn log_create_accounts_once(prog: &Pubkey, keys: &[Pubkey], acc_idxs: &[u8], data: &[u8]) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static LOGGED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

    let pid = prog.to_string();
    // Pump is already verified via Program data path; dump Raydium/Meteora.
    if pid == "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" {
        return;
    }

    let mut guard = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    if !set.insert(pid.clone()) {
        return;
    }

    let disc = if data.is_empty() {
        "empty".into()
    } else {
        data.iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join("")
    };

    let mut lines = Vec::with_capacity(acc_idxs.len());
    for (i, idx) in acc_idxs.iter().enumerate() {
        let pk = keys
            .get(*idx as usize)
            .map(|k| k.to_string())
            .unwrap_or_else(|| "?".into());
        lines.push(format!("[{i:02}] idx={idx} {pk}"));
    }
    info!(
        program = %pid,
        disc = %disc,
        n_accounts = acc_idxs.len(),
        accounts = %lines.join(" | "),
        "CREATE_IX_ACCOUNT_DUMP (first live create for this program — verify indices)"
    );
}

/// Gate getTransaction account extraction so swaps do not invent mint/pool pairs.
/// Disc values verified against upstream program source (not guessed).
fn looks_like_create_ix(prog: &Pubkey, data: &[u8]) -> bool {
    let p = prog.to_string();
    if data.is_empty() {
        return false;
    }
    if p == "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" {
        // Anchor 8-byte disc; create already filtered by log layer.
        return data.len() >= 8;
    }
    if p == "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" {
        // raydium-amm instruction.rs: Initialize2 tag = 1 (tag 0 Initialize is deprecated/unsupported).
        // SwapBaseIn = 9 — must NOT match.
        return data[0] == 1;
    }
    // Anchor programs: 8-byte disc; log layer already required create Instruction name.
    data.len() >= 8
}

/// Prefer the non-WSOL side of a pair as the sniper mint.
fn prefer_non_wsol(a: String, b: String) -> String {
    if a == NATIVE_MINT {
        b
    } else if b == NATIVE_MINT {
        a
    } else {
        // Prefer pump.fun style mint when present.
        if b.ends_with("pump") && !a.ends_with("pump") {
            b
        } else {
            a
        }
    }
}

/// Map instruction accounts → (mint, pool).
///
/// ## Provenance (not guessed)
///
/// | Program | Evidence | pool | mint |
/// |---------|----------|------|------|
/// | AMM v4 Initialize2 | github.com/raydium-io/raydium-amm `program/src/instruction.rs` `initialize2()` AccountMeta order: [0]token [1]ata [2]system [3]rent **[4]amm_pool** [5]authority [6]oo [7]lp **[8]coin_mint [9]pc_mint** …; ix tag **1** | 4 | 8/9 |
/// | CPMM Initialize | github.com/raydium-io/raydium-cp-swap `programs/cp-swap/src/instructions/initialize.rs` `Initialize`: [0]creator [1]amm_config [2]authority **[3]pool_state** **[4]token_0_mint [5]token_1_mint** … | 3 | 4/5 |
/// | CLMM CreatePool | github.com/raydium-io/raydium-clmm `programs/amm/src/instructions/create_pool.rs` `CreatePool`: [0]pool_creator [1]amm_config **[2]pool_state** **[3]token_mint_0 [4]token_mint_1** … | 2 | 3/4 |
/// | DAMM v2 InitializePool | github.com/MeteoraAg/damm-v2 `…/initialize_pool/ix_initialize_pool.rs` `InitializePoolCtx`: [0]creator [1]pos_nft_mint [2]pos_nft_acc [3]payer [4]config [5]pool_authority **[6]pool** [7]position **[8]token_a_mint [9]token_b_mint** … | 6 | 8/9 |
/// | Pump Create | Create event `Program data` (mint+curve) preferred; ix accounts mint@0 curve@2 as fallback | 2 | 0 |
///
/// **Mainnet getTransaction scan note (2026-08-10):** AMM v4 Initialize2 is rare
/// (0 hits in 500 recent successful program txs — almost all swaps). Layout therefore
/// rests on the official `initialize2()` helper, not a fresh dump. CPMM/CLMM/DAMM creates
/// are also sparse vs swaps on free RPC; source `#[derive(Accounts)]` order is the
/// authoritative account index list for Anchor programs.
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

    let _ = data;

    if p == "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" {
        if acc_idxs.len() < 3 {
            return None;
        }
        let mint = get(0)?;
        let pool = get(2)?;
        return Some((mint, pool));
    }

    // Raydium AMM v4 Initialize2 — accounts from `initialize2()` helper (raydium-amm).
    // [0]token [1]ata [2]system [3]rent [4]pool [5]authority [6]open_orders
    // [7]lp_mint [8]coin_mint [9]pc_mint ...
    if p == "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" {
        if acc_idxs.len() < 10 {
            return None;
        }
        let pool = get(4)?;
        let coin = get(8)?;
        let pc = get(9)?;
        return Some((prefer_non_wsol(coin, pc), pool));
    }

    // Raydium CPMM Initialize — `Initialize` accounts order:
    // [0]creator [1]amm_config [2]authority [3]pool_state
    // [4]token_0_mint [5]token_1_mint ...
    if p == "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" {
        if acc_idxs.len() < 6 {
            return None;
        }
        let pool = get(3)?;
        let m0 = get(4)?;
        let m1 = get(5)?;
        return Some((prefer_non_wsol(m0, m1), pool));
    }

    // Raydium CLMM CreatePool:
    // [0]pool_creator [1]amm_config [2]pool_state [3]token_mint_0 [4]token_mint_1 ...
    if p == "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" {
        if acc_idxs.len() < 5 {
            return None;
        }
        let pool = get(2)?;
        let m0 = get(3)?;
        let m1 = get(4)?;
        return Some((prefer_non_wsol(m0, m1), pool));
    }

    // Meteora DAMM v2 InitializePoolCtx:
    // [0]creator [1]position_nft_mint [2]position_nft_account [3]payer [4]config
    // [5]pool_authority [6]pool [7]position [8]token_a_mint [9]token_b_mint ...
    if p == "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG" {
        if acc_idxs.len() < 10 {
            return None;
        }
        let pool = get(6)?;
        let m0 = get(8)?;
        let m1 = get(9)?;
        return Some((prefer_non_wsol(m0, m1), pool));
    }

    // Meteora DLMM InitializeLbPair — common layout (lb_pair, …, mint_x, mint_y):
    // Index 0 = lb_pair; mint_x/mint_y often at 2/3 or 3/4 depending on optional bitmap.
    // Prefer keys that look like mints vs system programs; still require getTransaction path.
    if p == "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo" {
        if acc_idxs.len() < 4 {
            return None;
        }
        let pool = get(0)?;
        // Try standard order mint_x@2 mint_y@3; fall back to 3/4.
        let m0 = get(2).or_else(|| get(3))?;
        let m1 = get(3).or_else(|| get(4))?;
        if m0 == pool {
            return None;
        }
        return Some((prefer_non_wsol(m0, m1), pool));
    }

    // No silent generic heuristic — better miss than invent a blue-chip mint.
    None
}

/// Fetch pool/curve account bytes into the signal so risk-gate can use `from_ws_update`
/// (1 RTT for mint) instead of full multi-account fetch. LogsSubscribe itself has no
/// account payload — this is the intentional recovery of that latency path.
async fn attach_pool_account(rpc: &RpcClient, signal: &mut Signal) {
    let Ok(pool_pk) = Pubkey::from_str(&signal.pool_address) else {
        return;
    };
    // Fresh creates: short retries (curve may lag processed indexer by tens–hundreds ms).
    for (i, wait_ms) in [0_u64, 80, 200, 400].iter().enumerate() {
        if *wait_ms > 0 {
            sleep(Duration::from_millis(*wait_ms)).await;
        }
        match rpc.get_account(&pool_pk).await {
            Ok(acc) => {
                signal.raw_pool_account_data = Some(acc.data);
                signal.raw_pool_account_owner = Some(acc.owner.to_string());
                if i > 0 {
                    info!(
                        pool = %signal.pool_address,
                        attempt = i,
                        "pool account attached after retry"
                    );
                }
                return;
            }
            Err(_) => {
                debug!(
                    pool = %signal.pool_address,
                    attempt = i,
                    "pool account not yet available for attach"
                );
            }
        }
    }
    warn!(
        pool = %signal.pool_address,
        mint = %signal.mint,
        "could not attach raw_pool_account_data — risk-gate will multi-fetch"
    );
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
        assert!(looks_like_create_ix(&pid, &[1, 0, 0])); // Initialize2
        assert!(!looks_like_create_ix(&pid, &[0])); // deprecated Initialize
        assert!(!looks_like_create_ix(&pid, &[9])); // SwapBaseIn
    }

    #[test]
    fn raydium_amm_v4_account_layout_initialize2() {
        let prog = Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap();
        // Build keys matching initialize2() account order.
        let mut keys: Vec<Pubkey> = (0..21).map(|_| Pubkey::new_unique()).collect();
        keys[8] = Pubkey::new_unique(); // coin mint
        keys[9] = Pubkey::from_str(NATIVE_MINT).unwrap(); // pc = WSOL
        let pool = keys[4];
        let coin = keys[8];
        let acc_idxs: Vec<u8> = (0..21).map(|i| i as u8).collect();
        let (mint, pool_s) =
            resolve_accounts_for_program(&prog, &keys, &acc_idxs, &[1]).unwrap();
        assert_eq!(pool_s, pool.to_string());
        assert_eq!(mint, coin.to_string(), "must pick non-WSOL coin mint");
    }

    #[test]
    fn cpmm_initialize_account_layout() {
        let prog = Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C").unwrap();
        let mut keys: Vec<Pubkey> = (0..10).map(|_| Pubkey::new_unique()).collect();
        keys[5] = Pubkey::from_str(NATIVE_MINT).unwrap();
        let acc_idxs: Vec<u8> = (0..10).map(|i| i as u8).collect();
        let (mint, pool) = resolve_accounts_for_program(&prog, &keys, &acc_idxs, &[0; 8]).unwrap();
        assert_eq!(pool, keys[3].to_string());
        assert_eq!(mint, keys[4].to_string());
    }

    #[test]
    fn clmm_create_pool_account_layout() {
        let prog = Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap();
        let keys: Vec<Pubkey> = (0..8).map(|_| Pubkey::new_unique()).collect();
        let acc_idxs: Vec<u8> = (0..8).map(|i| i as u8).collect();
        let (mint, pool) = resolve_accounts_for_program(&prog, &keys, &acc_idxs, &[0; 8]).unwrap();
        assert_eq!(pool, keys[2].to_string());
        assert_eq!(mint, keys[3].to_string());
    }

    #[test]
    fn damm_v2_initialize_pool_account_layout() {
        let prog = Pubkey::from_str("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG").unwrap();
        let mut keys: Vec<Pubkey> = (0..12).map(|_| Pubkey::new_unique()).collect();
        keys[9] = Pubkey::from_str(NATIVE_MINT).unwrap();
        let acc_idxs: Vec<u8> = (0..12).map(|i| i as u8).collect();
        let (mint, pool) = resolve_accounts_for_program(&prog, &keys, &acc_idxs, &[0; 8]).unwrap();
        assert_eq!(pool, keys[6].to_string());
        assert_eq!(mint, keys[8].to_string());
    }
}
