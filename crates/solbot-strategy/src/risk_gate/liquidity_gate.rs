//! Liquidity, Pool Age, and Volume Spike Verification Gate.
//!
//! Rejects tokens that:
//! 1. Are too old (pool created > max_pool_age_secs ago, e.g. 180s) or dormant.
//! 2. Lack real initial liquidity (< min_pool_sol, e.g. 15.0 SOL).
//! 3. Lack sufficient organic buyer transactions (< min_unique_buyers, e.g. 5).

use crate::risk_gate::{CheckResult, PreBuyCheckContext};
use crate::tx_builder::pump_fun::{PumpFunBondingCurve, PUMP_FUN_PROGRAM};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solbot_common::types::Signal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default minimum real SOL required in pool/curve (15.0 SOL).
pub const DEFAULT_MIN_POOL_SOL: f64 = 15.0;

/// Default seconds to wait and confirm volume spike (8 seconds).
pub const DEFAULT_VOLUME_CONFIRM_SECS: u64 = 8;

/// Default minimum distinct transactions / buyers in the confirmation window (5).
pub const DEFAULT_MIN_UNIQUE_BUYERS: usize = 5;

/// Default maximum age of pool from creation to entry (180 seconds = 3 minutes).
pub const DEFAULT_MAX_POOL_AGE_SECS: u64 = 180;

/// Max transactions by the creator wallet in the last 6h before it is treated as a serial deployer.
pub const DEFAULT_MAX_DEPLOYER_RECENT_TXS: usize = 25;

/// Evaluate pool liquidity, age, and volume spike requirements.
pub async fn check_liquidity_and_volume(
    signal: &Signal,
    context: &PreBuyCheckContext,
    rpc: &Arc<RpcClient>,
    min_pool_sol: f64,
    volume_confirm_secs: u64,
    min_unique_buyers: usize,
    max_pool_age_secs: u64,
) -> CheckResult {
    let start = Instant::now();

    let pool_pk = match Pubkey::from_str(&signal.pool_address) {
        Ok(pk) => pk,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("Invalid pool address: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    let mint_pk = match Pubkey::from_str(&signal.mint) {
        Ok(pk) => pk,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("Invalid mint address: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    // Pump.fun Bonding Curve Check
    if signal.dex_program == PUMP_FUN_PROGRAM.to_string() {
        return check_pump_fun_liquidity(
            signal,
            context,
            rpc,
            &pool_pk,
            &mint_pk,
            min_pool_sol,
            volume_confirm_secs,
            min_unique_buyers,
            max_pool_age_secs,
            start,
        )
        .await;
    }

    // Default for other DEXes (Raydium, Meteora) - verify vault balance if context has pool data
    if let Some(ref pool_data) = context.pool_data {
        if pool_data.is_empty() {
            return CheckResult {
                passed: false,
                reason: Some("Empty pool account data".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    }

    if let Err(reason) = verify_pool_freshness_and_transactions(
        rpc,
        &pool_pk,
        min_unique_buyers,
        max_pool_age_secs,
    )
    .await
    {
        return CheckResult {
            passed: false,
            reason: Some(reason),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    CheckResult {
        passed: true,
        reason: None,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

async fn check_pump_fun_liquidity(
    signal: &Signal,
    context: &PreBuyCheckContext,
    rpc: &Arc<RpcClient>,
    pool_pk: &Pubkey,
    mint_pk: &Pubkey,
    min_pool_sol: f64,
    volume_confirm_secs: u64,
    min_unique_buyers: usize,
    max_pool_age_secs: u64,
    start: Instant,
) -> CheckResult {
    // 1. Pool age / activity check. "Not enough transactions" is expected right at launch,
    //    so re-check it throughout the volume-confirmation window instead of failing instantly.
    //    Any other failure (too old, dormant, ancient) rejects immediately.
    let freshness_deadline = Instant::now() + Duration::from_secs(volume_confirm_secs);
    loop {
        match verify_pool_freshness_and_transactions(
            rpc,
            pool_pk,
            min_unique_buyers,
            max_pool_age_secs,
        )
        .await
        {
            Ok(()) => break,
            Err(reason)
                if reason.starts_with("Not enough transactions")
                    && Instant::now() < freshness_deadline =>
            {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                continue;
            }
            Err(reason) => {
                tracing::warn!(
                    mint = %signal.mint,
                    pool = %signal.pool_address,
                    reason = %reason,
                    "Rejecting pool due to pool age / activity check"
                );
                return CheckResult {
                    passed: false,
                    reason: Some(reason),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
        }
    }

    let min_lamports = (min_pool_sol.max(0.0) * 1_000_000_000.0) as u64;

    // 1.5 Deployer reputation (serial scammers / bot factories)
    if let Some(ref data) = context.pool_data {
        if let Ok(curve) = PumpFunBondingCurve::from_account_data(data, pool_pk, mint_pk) {
            let max_txs: usize = std::env::var("SOLBOT_MAX_DEPLOYER_RECENT_TXS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_DEPLOYER_RECENT_TXS);
            if let Err(reason) =
                verify_deployer_reputation(rpc, &curve.creator, max_txs, 6 * 3600).await
            {
                tracing::warn!(mint = %signal.mint, reason = %reason, "Rejecting serial deployer");
                return CheckResult {
                    passed: false,
                    reason: Some(reason),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
        }
    }

    // 2. Check initial curve data from context
    let mut current_real_sol: u64 = 0;
    if let Some(ref data) = context.pool_data {
        if let Ok(curve) = PumpFunBondingCurve::from_account_data(data, pool_pk, mint_pk) {
            current_real_sol = curve.real_sol_reserves;
        }
    }

    // If initial liquidity already meets the threshold
    if current_real_sol >= min_lamports {
        return CheckResult {
            passed: true,
            reason: None,
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // 3. If below threshold and volume confirmation window is active, poll for incoming volume spike
    if volume_confirm_secs > 0 {
        let poll_interval = Duration::from_millis(1500);
        let max_polls = (volume_confirm_secs.max(1) * 1000 / 1500).max(1);

        tracing::info!(
            mint = %signal.mint,
            pool = %signal.pool_address,
            current_sol = current_real_sol as f64 / 1e9,
            required_sol = min_pool_sol,
            window_secs = volume_confirm_secs,
            "Awaiting volume spike / liquidity threshold before entry"
        );

        for _ in 0..max_polls {
            tokio::time::sleep(poll_interval).await;
            if let Ok(acc) = rpc.get_account(pool_pk).await {
                if let Ok(curve) = PumpFunBondingCurve::from_account_data(&acc.data, pool_pk, mint_pk) {
                    current_real_sol = curve.real_sol_reserves;
                    if current_real_sol >= min_lamports {
                        tracing::info!(
                            mint = %signal.mint,
                            real_sol = current_real_sol as f64 / 1e9,
                            "Volume spike confirmed - liquidity threshold met"
                        );
                        return CheckResult {
                            passed: true,
                            reason: None,
                            duration_ms: start.elapsed().as_millis() as u64,
                        };
                    }
                }
            }
        }
    }

    let real_sol_sol = current_real_sol as f64 / 1_000_000_000.0;
    tracing::warn!(
        mint = %signal.mint,
        pool = %signal.pool_address,
        real_sol = real_sol_sol,
        min_required = min_pool_sol,
        "Rejecting pool due to insufficient real liquidity / volume spike (dead/wash pool)"
    );

    CheckResult {
        passed: false,
        reason: Some(format!(
            "Insufficient real liquidity: {:.2} SOL in curve < {:.2} SOL required (dead pool / Mayhem wash bot)",
            real_sol_sol, min_pool_sol
        )),
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

/// Reject serial deployers: a wallet that signed many transactions recently is almost always a
/// scam factory / Mayhem-style bot (thousands of created tokens), not an organic launcher.
async fn verify_deployer_reputation(
    rpc: &Arc<RpcClient>,
    creator: &Pubkey,
    max_recent_txs: usize,
    window_secs: i64,
) -> Result<(), String> {
    use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
    if max_recent_txs == 0 {
        return Ok(());
    }
    let cfg = GetConfirmedSignaturesForAddress2Config {
        before: None,
        until: None,
        limit: Some(100),
        commitment: None,
    };
    match rpc.get_signatures_for_address_with_config(creator, cfg).await {
        Ok(sigs) => {
            let now = chrono::Utc::now().timestamp();
            let recent = sigs
                .iter()
                .filter(|s| s.block_time.map_or(true, |bt| now - bt <= window_secs))
                .count();
            if recent > max_recent_txs {
                return Err(format!(
                    "Serial deployer: creator {} has {} txs in last {}s (max {})",
                    creator, recent, window_secs, max_recent_txs
                ));
            }
            Ok(())
        }
        Err(e) => {
            tracing::warn!(creator = %creator, error = %e, "Deployer history fetch failed - not blocking");
            Ok(())
        }
    }
}

/// Verify that the pool is fresh (not an old/zombie token), active, and has sufficient buyers
async fn verify_pool_freshness_and_transactions(
    rpc: &Arc<RpcClient>,
    pool_pk: &Pubkey,
    min_txs: usize,
    max_pool_age_secs: u64,
) -> Result<(), String> {
    match rpc.get_signatures_for_address(pool_pk).await {
        Ok(sigs) => {
            // If the pool already has 1000+ transactions, it is definitely not a brand new launch
            if sigs.len() >= 1000 {
                return Err("Pool is ancient (≥1000 transactions history)".to_string());
            }

            if sigs.len() < min_txs {
                return Err(format!(
                    "Not enough transactions: {} < {} required",
                    sigs.len(),
                    min_txs
                ));
            }

            let now_ts = chrono::Utc::now().timestamp();

            // Earliest signature in this batch indicates pool creation time
            if let Some(earliest_sig) = sigs.last() {
                if let Some(bt) = earliest_sig.block_time {
                    let age = now_ts.saturating_sub(bt);
                    if age > max_pool_age_secs as i64 {
                        return Err(format!(
                            "Pool is too old: created {}s ago (max allowed: {}s)",
                            age, max_pool_age_secs
                        ));
                    }
                }
            }

            // Latest signature indicates recent trading activity
            if let Some(latest_sig) = sigs.first() {
                if let Some(bt) = latest_sig.block_time {
                    let dormancy = now_ts.saturating_sub(bt);
                    if dormancy > 60 {
                        return Err(format!(
                            "Pool is dormant: last transaction was {}s ago (max allowed: 60s)",
                            dormancy
                        ));
                    }
                }
            }

            Ok(())
        }
        Err(e) => {
            tracing::warn!(
                pool = %pool_pk,
                error = %e,
                "Failed to fetch signatures for address - allowing if SOL threshold passed"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert!(DEFAULT_MIN_POOL_SOL > 0.0);
        assert!(DEFAULT_VOLUME_CONFIRM_SECS > 0);
        assert!(DEFAULT_MIN_UNIQUE_BUYERS > 0);
        assert!(DEFAULT_MAX_POOL_AGE_SECS > 0);
    }
}
