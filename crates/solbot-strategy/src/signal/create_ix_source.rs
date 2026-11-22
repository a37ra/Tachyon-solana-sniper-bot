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
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::Signature;
use solana_transaction_status::{option_serializer::OptionSerializer, UiTransactionEncoding};
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

    // Optional HTTP RPC fallback poller — disabled by default when WebSocket is active
    // because polling getSignaturesForAddress + 10x getTransaction every 6s burns ~66k Helius credits/hr.
    let enable_http_poller = std::env::var("SOLBOT_ENABLE_HTTP_POLLER")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if enable_http_poller {
        let rpc_http = rpc.clone();
        let tx_http = tx.clone();
        let seen_http = seen.clone();
        tokio::spawn(async move {
            run_http_pump_poller(rpc_http, tx_http, seen_http).await;
        });
    }

    let relay_tx: Option<tokio::sync::broadcast::Sender<String>> =
        if let Ok(bind_addr) = std::env::var("SOLBOT_WS_RELAY_BIND") {
            let bind_addr = bind_addr.trim().to_string();
            if !bind_addr.is_empty() {
                let (btx, _) = tokio::sync::broadcast::channel::<String>(2048);
                let btx_srv = btx.clone();
                tokio::spawn(async move {
                    match tokio::net::TcpListener::bind(&bind_addr).await {
                        Ok(listener) => {
                            info!(bind = %bind_addr, "Local WS fan-out relay listening for secondary bot instances");
                            while let Ok((stream, peer)) = listener.accept().await {
                                let mut brx = btx_srv.subscribe();
                                tokio::spawn(async move {
                                    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                                        return;
                                    };
                                    info!(?peer, "Local WS fan-out client connected");
                                    let (mut w, mut r) = ws.split();
                                    loop {
                                        tokio::select! {
                                             biased;
                                            frame = brx.recv() => {
                                                match frame {
                                                    Ok(txt) => {
                                                        if w.send(Message::Text(txt)).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                                    Err(_) => break,
                                                }
                                            }
                                            inc = r.next() => {
                                                match inc {
                                                    Some(Ok(Message::Ping(p))) => {
                                                        if w.send(Message::Pong(p)).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                    Some(Ok(_)) => {}
                                                    _ => break,
                                                }
                                            }
                                        }
                                    }
                                });
                            }
                        }
                        Err(e) => warn!(bind = %bind_addr, error = %e, "Failed to bind local WS fan-out relay"),
                    }
                });
                Some(btx)
            } else {
                None
            }
        } else {
            None
        };

    tokio::spawn(async move {
        let mut attempts = 0u32;
        let fallback_ws = std::env::var("SOLBOT_FALLBACK_WS_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        // Budget for paid fallback WS (Helius) so an outage cannot burn credits: max secs per hour.
        let fallback_budget_secs: u64 = std::env::var("SOLBOT_FALLBACK_BUDGET_SECS_PER_HOUR")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);
        let fallback_session_cap = Duration::from_secs(90);
        let mut budget_window_start = std::time::Instant::now();
        let mut fallback_used_secs: u64 = 0;
        let jitter_ms = |max: u64| -> u64 {
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0))
                % max.max(1)
        };
        loop {
            if budget_window_start.elapsed() >= Duration::from_secs(3600) {
                budget_window_start = std::time::Instant::now();
                fallback_used_secs = 0;
            }
            let use_fallback = attempts >= 5
                && fallback_ws.is_some()
                && fallback_used_secs < fallback_budget_secs;
            let active_ws = if use_fallback {
                fallback_ws.as_deref().unwrap_or(&ws_url)
            } else {
                &ws_url
            };
            let t0 = std::time::Instant::now();
            let run = run_logs_loop(active_ws, &program_ids, rpc.clone(), &tx, &seen, relay_tx.as_ref());
            let res = if use_fallback {
                warn!(ws = active_ws, cap_s = fallback_session_cap.as_secs(), "Primary WS failing — using budgeted fallback WS");
                match tokio::time::timeout(fallback_session_cap, run).await {
                    Ok(r) => r,
                    Err(_) => Err(anyhow!("fallback session cap reached — returning to primary")),
                }
            } else {
                run.await
            };
            if use_fallback {
                fallback_used_secs += t0.elapsed().as_secs();
                attempts = 0;
            }
            match res {
                Ok(()) => return,
                Err(e) => {
                    if use_fallback {
                        sleep(Duration::from_millis(300)).await;
                    } else if t0.elapsed() >= Duration::from_secs(10) {
                        attempts = 0;
                        debug!(error = %e, ws = active_ws, elapsed_s = t0.elapsed().as_secs(), "Public WS session rotated — reconnecting");
                        sleep(Duration::from_millis(300 + jitter_ms(700))).await;
                    } else {
                        attempts = attempts.saturating_add(1);
                        // Jittered backoff 2s..10s; public node rate-limits tight reconnect loops (HTTP 413).
                        let base = (2u64 << attempts.min(3)).min(10);
                        let backoff = Duration::from_millis(base * 1000 + jitter_ms(1500));
                        error!(error = %e, attempt = attempts, ws = active_ws, ?backoff, "create-ix WS reconnect");
                        sleep(backoff).await;
                    }
                }
            }
        }
    });

    Ok(rx)
}

async fn run_http_pump_poller(
    rpc: Arc<RpcClient>,
    tx: mpsc::Sender<Signal>,
    seen: SeenPools,
) {
    use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
    use solana_message::VersionedMessage;
    use std::collections::HashSet;

    let Ok(pump_pid) = Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P") else {
        return;
    };
    let mut seen_sigs: HashSet<String> = HashSet::new();
    let mut poll_interval = interval(Duration::from_secs(6));
    info!("HTTP RPC fallback signal poller started (free-tier resilient mode)");

    loop {
        poll_interval.tick().await;
        let cfg = GetConfirmedSignaturesForAddress2Config {
            before: None,
            until: None,
            limit: Some(10),
            commitment: Some(CommitmentConfig::confirmed()),
        };
        let Ok(sigs) = rpc
            .get_signatures_for_address_with_config(&pump_pid, cfg)
            .await
        else {
            continue;
        };

        for sig_info in sigs {
            if sig_info.err.is_some() {
                continue;
            }
            if !seen_sigs.insert(sig_info.signature.clone()) {
                continue;
            }
            let Ok(sig) = Signature::from_str(&sig_info.signature) else {
                continue;
            };
            let tx_cfg = RpcTransactionConfig {
                encoding: Some(UiTransactionEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                max_supported_transaction_version: Some(0),
            };
            let Ok(conf) = rpc.get_transaction_with_config(&sig, tx_cfg).await else {
                continue;
            };

            // Only accept genuine pool creation transactions, not ordinary trades on old pools
            let is_create_tx = if let Some(ref meta) = conf.transaction.meta {
                if let OptionSerializer::Some(ref logs) = meta.log_messages {
                    logs.iter().any(|l| l.contains("Instruction: Create"))
                } else {
                    false
                }
            } else {
                false
            };
            if !is_create_tx {
                continue;
            }

            let mut all_keys = Vec::new();
            let encoded = conf.transaction.transaction;
            let bytes_opt = match encoded {
                solana_transaction_status::EncodedTransaction::Binary(b64, _)
                | solana_transaction_status::EncodedTransaction::LegacyBinary(b64) => {
                    base64::engine::general_purpose::STANDARD.decode(b64).ok()
                }
                _ => None,
            };
            let Some(bytes) = bytes_opt else {
                continue;
            };

            if let Ok(vtx) =
                bincode::deserialize::<solana_transaction::versioned::VersionedTransaction>(&bytes)
            {
                all_keys.extend_from_slice(vtx.message.static_account_keys());
            } else if let Ok(ltx) = bincode::deserialize::<solana_transaction::Transaction>(&bytes)
            {
                let vmsg = VersionedMessage::Legacy(ltx.message);
                all_keys.extend_from_slice(vmsg.static_account_keys());
            }

            if let Some(ref meta) = conf.transaction.meta {
                if let OptionSerializer::Some(ref loaded) = meta.loaded_addresses {
                    for k in loaded.writable.iter().chain(loaded.readonly.iter()) {
                        if let Ok(pk) = Pubkey::from_str(k) {
                            all_keys.push(pk);
                        }
                    }
                }
            }

            let key_set: HashSet<Pubkey> = all_keys.iter().copied().collect();
            let mut found_pair = None;
            for candidate_mint in &all_keys {
                let (expected_curve, _) = Pubkey::find_program_address(
                    &[b"bonding-curve", candidate_mint.as_ref()],
                    &pump_pid,
                );
                if key_set.contains(&expected_curve) {
                    found_pair = Some((candidate_mint.to_string(), expected_curve.to_string()));
                    break;
                }
            }

            if let Some((mint_str, pool_str)) = found_pair {
                if let Some(mut signal) = finalize_signal(
                    mint_str,
                    pool_str,
                    pump_pid.to_string(),
                    sig_info.slot,
                    &seen,
                ) {
                    attach_pool_account(rpc.as_ref(), &mut signal).await;
                    signal.received_at = chrono::Utc::now().timestamp_millis() as u64;
                    info!(
                        mint = %signal.mint,
                        pool = %signal.pool_address,
                        slot = sig_info.slot,
                        "HTTP RPC signal resolved (Pump.fun bonding curve)"
                    );
                    let _ = tx.send(signal).await;
                    break;
                }
            }
        }
    }
}

async fn run_logs_loop(
    ws_url: &str,
    program_ids: &[Pubkey],
    rpc: Arc<RpcClient>,
    tx: &mpsc::Sender<Signal>,
    seen: &SeenPools,
    relay_tx: Option<&tokio::sync::broadcast::Sender<String>>,
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

    let is_public_rpc = std::env::var("SOLBOT_PUBLIC_RPC")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let default_gap_ms = if is_public_rpc { 600 } else { 0 };
    let gap_ms = std::env::var("SOLBOT_SIGNAL_GAP_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default_gap_ms);
    let min_signal_gap = Duration::from_millis(gap_ms);
    let mut last_signal_emitted = std::time::Instant::now()
        .checked_sub(Duration::from_secs(10))
        .unwrap_or_else(std::time::Instant::now);

    let mut ping = interval(Duration::from_secs(30));
    let mut last_ws_msg = std::time::Instant::now();
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if last_ws_msg.elapsed() > Duration::from_secs(90) {
                    warn!("create-ix WS silent for >90s — forcing reconnect");
                    anyhow::bail!("websocket silent timeout");
                }
                if let Err(e) = write.send(Message::Ping(vec![])).await {
                    warn!(error = %e, "create-ix WS ping failed — forcing reconnect");
                    anyhow::bail!("websocket ping failed: {e}");
                }
            }
            msg = read.next() => {
                last_ws_msg = std::time::Instant::now();
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(rtx) = relay_tx {
                            let _ = rtx.send(text.clone());
                        }
                        crate::signal::trade_recorder::record_raw(&text);
                        for snap in crate::signal::trade_recorder::take_ready_breakout_signals() {
                            if let Some(mut signal) = finalize_signal(
                                snap.mint.clone(),
                                snap.pool.clone(),
                                crate::tx_builder::pump_fun::PUMP_FUN_PROGRAM.to_string(),
                                snap.last_slot,
                                seen,
                            ) {
                                attach_pool_account(rpc.as_ref(), &mut signal).await;
                                if let Some(ref raw_data) = signal.raw_pool_account_data {
                                    if let (Ok(pool_pk), Ok(mint_pk)) = (
                                        Pubkey::from_str(&signal.pool_address),
                                        Pubkey::from_str(&signal.mint),
                                    ) {
                                        if let Ok(curve) = crate::tx_builder::pump_fun::PumpFunBondingCurve::from_account_data(
                                            raw_data,
                                            &pool_pk,
                                            &mint_pk,
                                        ) {
                                            if curve.complete
                                                || !crate::risk_gate::liquidity_gate::is_standard_sol_curve(&curve)
                                            {
                                                continue;
                                            }
                                        }
                                    }
                                }
                                last_signal_emitted = std::time::Instant::now();
                                signal.received_at = chrono::Utc::now().timestamp_millis() as u64;
                                info!(
                                    mint = %signal.mint,
                                    pool = %signal.pool_address,
                                    real_sol = snap.real_sol,
                                    slot0_sol = snap.slot0_sol,
                                    organic_sol = snap.organic_sol,
                                    organic_buyers = snap.organic_buyers,
                                    organic_sr = snap.organic_sell_ratio,
                                    age_secs = snap.age_secs,
                                    has_raw_pool = signal.raw_pool_account_data.is_some(),
                                    "S4 WS anti-sybil crowd-breakout signal"
                                );
                                if tx.send(signal).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                        if crate::signal::trade_recorder::ws_breakout_trigger_enabled() {
                            continue;
                        }
                        if !min_signal_gap.is_zero() && last_signal_emitted.elapsed() < min_signal_gap {
                            continue;
                        }
                        let parsed = match logs_parser::parse_logs_message(&text) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        for ps in parsed {
                            if !ps.instruction_type.is_create_event() {
                                continue;
                            }
                            if !min_signal_gap.is_zero() && last_signal_emitted.elapsed() < min_signal_gap {
                                break;
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
                                        if signal.dex_program
                                            == crate::tx_builder::pump_fun::PUMP_FUN_PROGRAM
                                                .to_string()
                                        {
                                            if let Some(ref raw_data) = signal.raw_pool_account_data {
                                                if let (Ok(pool_pk), Ok(mint_pk)) = (
                                                    Pubkey::from_str(&signal.pool_address),
                                                    Pubkey::from_str(&signal.mint),
                                                ) {
                                                    if let Ok(curve) = crate::tx_builder::pump_fun::PumpFunBondingCurve::from_account_data(
                                                        raw_data,
                                                        &pool_pk,
                                                        &mint_pk,
                                                    ) {
                                                        if curve.complete
                                                            || !crate::risk_gate::liquidity_gate::is_standard_sol_curve(&curve)
                                                        {
                                                            debug!(
                                                                mint = %signal.mint,
                                                                complete = curve.complete,
                                                                "Skipping non-standard / completed Pump.fun curve early"
                                                            );
                                                            continue;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        last_signal_emitted = std::time::Instant::now();
                                        signal.received_at = chrono::Utc::now().timestamp_millis() as u64;
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

                            // On free/public RPC, skip heavy getTransaction resolution if rate-limited
                            let Some(sig_str) = ps.signature.as_ref() else { continue };
                            last_signal_emitted = std::time::Instant::now();
                            match resolve_from_tx(rpc.as_ref(), sig_str, ps.slot, program_ids, seen).await {
                                Ok(Some(mut signal)) => {
                                    attach_pool_account(rpc.as_ref(), &mut signal).await;
                                    signal.received_at = chrono::Utc::now().timestamp_millis() as u64;
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
    let tx_cfg = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Base64),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    };
    let conf = rpc
        .get_transaction_with_config(&sig, tx_cfg)
        .await
        .map_err(|e| anyhow!("getTransaction: {e}"))?;

    // Extract Address Lookup Table (ALT) loaded addresses from transaction meta for V0 txs
    let mut loaded_keys = Vec::new();
    if let Some(ref meta) = conf.transaction.meta {
        if let OptionSerializer::Some(ref loaded) = meta.loaded_addresses {
            for k in &loaded.writable {
                if let Ok(pk) = Pubkey::from_str(k) {
                    loaded_keys.push(pk);
                }
            }
            for k in &loaded.readonly {
                if let Ok(pk) = Pubkey::from_str(k) {
                    loaded_keys.push(pk);
                }
            }
        }
    }

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
        if let Some(s) = extract_from_versioned(&vtx, &loaded_keys, program_ids, slot, seen) {
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
    loaded_keys: &[Pubkey],
    program_ids: &[Pubkey],
    slot: u64,
    seen: &SeenPools,
) -> Option<Signal> {
    use solana_message::VersionedMessage;
    let mut all_keys = vtx.message.static_account_keys().to_vec();
    all_keys.extend_from_slice(loaded_keys);
    let keys = all_keys.as_slice();

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
    if p == "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo" {
        if acc_idxs.len() < 4 {
            return None;
        }
        let pool = get(0)?;
        let m0 = get(2).or_else(|| get(3))?;
        let m1 = get(3).or_else(|| get(4))?;
        if m0 == pool {
            return None;
        }
        return Some((prefer_non_wsol(m0, m1), pool));
    }

    None
}

/// Fetch pool/curve account bytes into the signal so risk-gate can use `from_ws_update`
/// (1 RTT for mint) instead of full multi-account fetch.
async fn attach_pool_account(rpc: &RpcClient, signal: &mut Signal) {
    let Ok(pool_pk) = Pubkey::from_str(&signal.pool_address) else {
        return;
    };
    for (i, wait_ms) in [120_u64, 280].iter().enumerate() {
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
    use crate::signal::pool_signal::SYSTEM_PROGRAM;

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

    #[test]
    fn v0_with_alt_loaded_addresses_resolves_create_ix() {
        use solana_message::{
            compiled_instruction::CompiledInstruction, v0::Message as V0Message, MessageHeader,
            VersionedMessage,
        };
        use solana_sdk::hash::Hash;
        use solana_transaction::versioned::VersionedTransaction;

        let cpmm_prog = Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C").unwrap();
        let payer = Pubkey::new_unique();
        // Only payer and program are in static_account_keys (indices 0, 1).
        // The 6 CPMM Initialize accounts are in ALT loaded_keys (indices 2..8).
        let static_keys = vec![payer, cpmm_prog];
        let mut loaded_keys: Vec<Pubkey> = (0..6).map(|_| Pubkey::new_unique()).collect();
        loaded_keys[5] = Pubkey::from_str(NATIVE_MINT).unwrap(); // token_1_mint = WSOL
        let expected_pool = loaded_keys[3];
        let expected_mint = loaded_keys[4];

        let compiled_ix = CompiledInstruction {
            program_id_index: 1,
            accounts: vec![2, 3, 4, 5, 6, 7],
            data: vec![175, 175, 109, 31, 13, 152, 155, 237], // 8-byte Anchor disc
        };

        let v0_msg = V0Message {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 1,
            },
            account_keys: static_keys,
            recent_blockhash: Hash::new_unique(),
            instructions: vec![compiled_ix],
            address_table_lookups: vec![],
        };

        let vtx = VersionedTransaction {
            signatures: vec![],
            message: VersionedMessage::V0(v0_msg),
        };

        let seen = SeenPools::with_warmup_secs(0);
        let sig = extract_from_versioned(&vtx, &loaded_keys, &[cpmm_prog], 100, &seen)
            .expect("should resolve V0 + ALT create ix");
        assert_eq!(sig.mint, expected_mint.to_string());
        assert_eq!(sig.pool_address, expected_pool.to_string());
    }
}
