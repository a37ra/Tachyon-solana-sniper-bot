//! Liquidity and Volume Spike Verification Gate.
//!
//! Rejects tokens that lack real initial liquidity or sufficient organic buyer activity
//! (e.g. empty pools, micro-wash trading by Mayhem bots, or 0.1 SOL dev rugs).

use crate::risk_gate::{CheckResult, PreBuyCheckContext};
use crate::tx_builder::pump_fun::{PumpFunBondingCurve, PUMP_FUN_PROGRAM};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solbot_common::types::Signal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default minimum real SOL required in pool/curve (10.0 SOL).
pub const DEFAULT_MIN_POOL_SOL: f64 = 10.0;

/// Default seconds to wait and confirm volume spike (5 seconds).
pub const DEFAULT_VOLUME_CONFIRM_SECS: u64 = 5;

/// Default minimum distinct transactions / buyers in the confirmation window (5).
pub const DEFAULT_MIN_UNIQUE_BUYERS: usize = 5;

/// Evaluate pool liquidity and volume spike requirements.
pub async fn check_liquidity_and_volume(
    signal: &Signal,
    context: &PreBuyCheckContext,
    rpc: &Arc<RpcClient>,
    min_pool_sol: f64,
    volume_confirm_secs: u64,
    min_unique_buyers: usize,
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
    start: Instant,
) -> CheckResult {
    let min_lamports = (min_pool_sol.max(0.0) * 1_000_000_000.0) as u64;

    // Check initial curve data from context
    let mut current_real_sol: u64 = 0;
    if let Some(ref data) = context.pool_data {
        if let Ok(curve) = PumpFunBondingCurve::from_account_data(data, pool_pk, mint_pk) {
            current_real_sol = curve.real_sol_reserves;
        }
    }

    // If initial liquidity already meets the threshold
    if current_real_sol >= min_lamports {
        let unique_ok = verify_unique_transactions(rpc, pool_pk, min_unique_buyers).await;
        if unique_ok {
            return CheckResult {
                passed: true,
                reason: None,
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    }

    // If below threshold and volume confirmation window is active, poll for incoming volume spike
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
                        let unique_ok = verify_unique_transactions(rpc, pool_pk, min_unique_buyers).await;
                        if unique_ok {
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

/// Verify that the pool has a minimum number of distinct transaction signatures
async fn verify_unique_transactions(
    rpc: &Arc<RpcClient>,
    pool_pk: &Pubkey,
    min_txs: usize,
) -> bool {
    if min_txs <= 1 {
        return true;
    }

    match rpc
        .get_signatures_for_address(pool_pk)
        .await
    {
        Ok(sigs) => {
            // Must have at least min_txs signatures
            sigs.len() >= min_txs
        }
        Err(e) => {
            tracing::warn!(
                pool = %pool_pk,
                error = %e,
                "Failed to fetch signatures for address - allowing if SOL threshold passed"
            );
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_pool_sol_constant() {
        assert!(DEFAULT_MIN_POOL_SOL > 0.0);
        assert!(DEFAULT_VOLUME_CONFIRM_SECS > 0);
        assert!(DEFAULT_MIN_UNIQUE_BUYERS > 0);
    }
}
