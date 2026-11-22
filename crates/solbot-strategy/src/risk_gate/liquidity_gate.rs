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

/// Default maximum initial real SOL allowed at discovery (t=0) to avoid pre-bundled dev pools.
pub const DEFAULT_MAX_INITIAL_POOL_SOL: f64 = 14.0;

/// Default maximum real SOL allowed post-inflow at entry to avoid overheated / near-graduation entries.
pub const DEFAULT_MAX_POOL_SOL: f64 = 22.0;

/// Default minimum net real SOL inflow required after signal discovery before entry.
pub const DEFAULT_MIN_INFLOW_SOL: f64 = 0.8;

/// Default maximum drawdown from peak real SOL tolerated during confirmation window (5%).
pub const DEFAULT_MAX_CONFIRM_DROP_PCT: f64 = 0.05;

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_u64_val(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize_val(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

/// Standard Pump.fun bonding curves start with 30 SOL virtual reserve offset (`virtual = 30 SOL + real`).
/// Non-standard curves (~4969 SOL virtual base) confuse SOL-denominated liquidity gates.
pub fn is_standard_sol_curve(curve: &PumpFunBondingCurve) -> bool {
    let virtual_base = curve.virtual_sol_reserves.saturating_sub(curve.real_sol_reserves);
    (29_950_000_000..=30_050_000_000).contains(&virtual_base)
}

static CONFIRMED_POOL_SOL: std::sync::OnceLock<dashmap::DashMap<String, u64>> =
    std::sync::OnceLock::new();

pub fn record_confirmed_pool_sol(pool_address: &str, real_sol_lamports: u64) {
    CONFIRMED_POOL_SOL
        .get_or_init(dashmap::DashMap::new)
        .insert(pool_address.to_string(), real_sol_lamports);
}

pub fn get_confirmed_pool_sol(pool_address: &str) -> Option<u64> {
    CONFIRMED_POOL_SOL
        .get()
        .and_then(|m| m.get(pool_address).map(|v| *v))
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
    // Read live env overrides so .env hot-reload updates risk-gate thresholds without restart.
    let min_pool_sol = env_f64("SOLBOT_MIN_POOL_SOL", min_pool_sol);
    let max_initial_pool_sol = env_f64("SOLBOT_MAX_INITIAL_POOL_SOL", DEFAULT_MAX_INITIAL_POOL_SOL);
    let max_pool_sol = env_f64("SOLBOT_MAX_POOL_SOL", DEFAULT_MAX_POOL_SOL);
    let min_inflow_sol = env_f64("SOLBOT_MIN_INFLOW_SOL", DEFAULT_MIN_INFLOW_SOL);
    let max_confirm_drop_pct = env_f64("SOLBOT_MAX_CONFIRM_DROP_PCT", DEFAULT_MAX_CONFIRM_DROP_PCT)
        .clamp(0.0, 0.99);
    let volume_confirm_secs = env_u64_val("SOLBOT_VOLUME_CONFIRM_SECS", volume_confirm_secs);
    let min_unique_buyers = env_usize_val("SOLBOT_MIN_UNIQUE_BUYERS", min_unique_buyers);
    let max_pool_age_secs = env_u64_val("SOLBOT_MAX_POOL_AGE_SECS", max_pool_age_secs);

    let min_lamports = (min_pool_sol.max(0.0) * 1_000_000_000.0) as u64;
    let max_initial_lamports = if max_initial_pool_sol > 0.0 {
        (max_initial_pool_sol * 1_000_000_000.0) as u64
    } else {
        u64::MAX
    };
    let max_lamports = if max_pool_sol > 0.0 {
        (max_pool_sol * 1_000_000_000.0) as u64
    } else {
        u64::MAX
    };
    let min_inflow_lamports = (min_inflow_sol.max(0.0) * 1_000_000_000.0) as u64;
    let min_organic_sol = env_f64("SOLBOT_MIN_ORGANIC_SOL", 9.0);
    let max_sell_ratio = env_f64("SOLBOT_MAX_SELL_RATIO", 0.30);
    let max_single_buy_sol = env_f64("SOLBOT_MAX_SINGLE_BUY_SOL", 0.0);
    let max_wallet_cum_sol = env_f64("SOLBOT_MAX_WALLET_CUM_SOL", 0.0);
    let max_slot_inflow_sol = env_f64("SOLBOT_MAX_SLOT_INFLOW_SOL", 0.0);
    let no_creator_sell = std::env::var("SOLBOT_NO_CREATOR_SELL")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let max_deployer_txs =
        env_usize_val("SOLBOT_MAX_DEPLOYER_RECENT_TXS", DEFAULT_MAX_DEPLOYER_RECENT_TXS);

    // Fast path: S4/S5 WebSocket TradeEvent anti-sybil crowd-breakout flow tracker
    if let Some(snap) = crate::signal::trade_recorder::get_mint_flow_snapshot(&signal.mint) {
        if snap.mayhem_or_nonstd {
            return CheckResult {
                passed: false,
                reason: Some("Mayhem / non-standard dynamic virtual reserve curve".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        if snap.creator_launch_num > 1 {
            return CheckResult {
                passed: false,
                reason: Some(format!(
                    "Repeat session creator: launch #{} by {}",
                    snap.creator_launch_num, snap.creator
                )),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        if no_creator_sell && snap.creator_sold {
            return CheckResult {
                passed: false,
                reason: Some("Creator already sold tokens before entry".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        if max_initial_pool_sol > 0.0 && snap.slot0_sol > max_initial_pool_sol {
            return CheckResult {
                passed: false,
                reason: Some(format!(
                    "Jito creation-slot bundle too large: {:.2} SOL > max {:.2} SOL",
                    snap.slot0_sol, max_initial_pool_sol
                )),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        if max_single_buy_sol > 0.0 && snap.max_single_org_buy_sol > max_single_buy_sol {
            return CheckResult {
                passed: false,
                reason: Some(format!(
                    "Anti-whale filter: single buy {:.2} SOL > max {:.2} SOL",
                    snap.max_single_org_buy_sol, max_single_buy_sol
                )),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        if max_wallet_cum_sol > 0.0 && snap.max_wallet_cum_sol > max_wallet_cum_sol {
            return CheckResult {
                passed: false,
                reason: Some(format!(
                    "Anti-whale filter: wallet cumulative buy {:.2} SOL > max {:.2} SOL",
                    snap.max_wallet_cum_sol, max_wallet_cum_sol
                )),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        if max_slot_inflow_sol > 0.0 && snap.max_slot_inflow_sol > max_slot_inflow_sol {
            return CheckResult {
                passed: false,
                reason: Some(format!(
                    "Anti-whale split-bundle filter: single-slot inflow {:.2} SOL > max {:.2} SOL",
                    snap.max_slot_inflow_sol, max_slot_inflow_sol
                )),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        if snap.breakout_triggered
            || (snap.organic_buyers >= min_unique_buyers
                && snap.organic_sol >= min_organic_sol
                && snap.real_sol >= min_pool_sol
                && snap.real_sol <= max_pool_sol
                && snap.inflow_win_sol >= min_inflow_sol
                && snap.drawdown_pct <= max_confirm_drop_pct
                && snap.organic_sell_ratio <= max_sell_ratio)
        {
            if let Some(reason) = check_cached_serial_deployer(&snap.creator) {
                return CheckResult {
                    passed: false,
                    reason: Some(reason),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
            if let Err(reason) =
                verify_deployer_reputation(rpc, &snap.creator, max_deployer_txs, 6 * 3600).await
            {
                return CheckResult {
                    passed: false,
                    reason: Some(reason),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
            let curve_data = match context.pool_data.clone() {
                Some(d) => Some(d),
                None => rpc.get_account(pool_pk).await.ok().map(|a| a.data),
            };
            if let Some(ref data) = curve_data {
                if let Ok(curve) = PumpFunBondingCurve::from_account_data(data, pool_pk, mint_pk) {
                    if curve.complete || !is_standard_sol_curve(&curve) {
                        return CheckResult {
                            passed: false,
                            reason: Some("Pool completed or non-standard on-chain".to_string()),
                            duration_ms: start.elapsed().as_millis() as u64,
                        };
                    }
                    if curve.real_sol_reserves > max_lamports.saturating_mul(125) / 100 {
                        return CheckResult {
                            passed: false,
                            reason: Some(format!(
                                "Pool overheated on-chain: {:.2} > {:.2} SOL",
                                curve.real_sol_reserves as f64 / 1e9,
                                max_pool_sol * 1.25
                            )),
                            duration_ms: start.elapsed().as_millis() as u64,
                        };
                    }
                    if curve.real_sol_reserves < min_lamports.saturating_mul(92) / 100 {
                        return CheckResult {
                            passed: false,
                            reason: Some(format!(
                                "Pool dumped on-chain before entry: {:.2} < {:.2} SOL",
                                curve.real_sol_reserves as f64 / 1e9,
                                min_pool_sol * 0.92
                            )),
                            duration_ms: start.elapsed().as_millis() as u64,
                        };
                    }
                    record_confirmed_pool_sol(&signal.pool_address, curve.real_sol_reserves);
                    tracing::info!(
                        mint = %signal.mint,
                        real_sol = curve.real_sol_reserves as f64 / 1e9,
                        slot0_sol = snap.slot0_sol,
                        organic_sol = snap.organic_sol,
                        organic_buyers = snap.organic_buyers,
                        organic_sr = snap.organic_sell_ratio,
                        inflow_win_sol = snap.inflow_win_sol,
                        age_secs = snap.age_secs,
                        "S4 anti-sybil crowd-breakout confirmed on-chain"
                    );
                    return CheckResult {
                        passed: true,
                        reason: None,
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                }
            }
            return CheckResult {
                passed: false,
                reason: Some("Failed to fetch on-chain curve data for S4 breakout".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        if crate::signal::trade_recorder::ws_breakout_trigger_enabled() {
            return CheckResult {
                passed: false,
                reason: Some("S4 breakout criteria not satisfied".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    }

    // 0. Capture initial curve snapshot (from WS context or single RPC fetch) and reject non-standard,
    //    already-graduated, or overheated initial bundles BEFORE waiting in polling loops.
    let initial_curve_data = match context.pool_data.clone() {
        Some(data) => Some(data),
        None => rpc.get_account(pool_pk).await.ok().map(|a| a.data),
    };

    let mut initial_real_sol: u64 = 0;
    let mut deferred_creator: Option<Pubkey> = None;
    let max_deployer_txs =
        env_usize_val("SOLBOT_MAX_DEPLOYER_RECENT_TXS", DEFAULT_MAX_DEPLOYER_RECENT_TXS);
    if let Some(ref data) = initial_curve_data {
        if let Ok(curve) = PumpFunBondingCurve::from_account_data(data, pool_pk, mint_pk) {
            if curve.complete {
                return CheckResult {
                    passed: false,
                    reason: Some("Pool already complete / migrated".to_string()),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
            if !is_standard_sol_curve(&curve) {
                let virtual_base_sol =
                    curve.virtual_sol_reserves.saturating_sub(curve.real_sol_reserves) as f64 / 1e9;
                return CheckResult {
                    passed: false,
                    reason: Some(format!(
                        "Non-standard virtual SOL base: {:.2} SOL (expected ~30 SOL)",
                        virtual_base_sol
                    )),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
            initial_real_sol = curve.real_sol_reserves;
            if initial_real_sol > max_initial_lamports {
                return CheckResult {
                    passed: false,
                    reason: Some(format!(
                        "Overheated initial pool / dev bundle: {:.2} SOL > max initial {:.2} SOL",
                        initial_real_sol as f64 / 1e9,
                        max_initial_pool_sol
                    )),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }

            // 1. Deployer reputation (serial scammers / bot factories):
            //    Always reject cached repeat scammers at t=0 with 0 RPC calls.
            //    For tiny < 2 SOL launches not in cache, defer RPC check until after curve confirmation
            //    so dead 0.01 SOL tokens don't burn 10 Helius credits each on getSignaturesForAddress.
            if let Some(reason) = check_cached_serial_deployer(&curve.creator) {
                tracing::warn!(mint = %signal.mint, reason = %reason, "Rejecting cached serial deployer");
                return CheckResult {
                    passed: false,
                    reason: Some(reason),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
            if initial_real_sol >= 2_000_000_000 {
                if let Err(reason) =
                    verify_deployer_reputation(rpc, &curve.creator, max_deployer_txs, 6 * 3600).await
                {
                    tracing::warn!(mint = %signal.mint, reason = %reason, "Rejecting serial deployer");
                    return CheckResult {
                        passed: false,
                        reason: Some(reason),
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                }
            } else {
                deferred_creator = Some(curve.creator);
            }
        }
    }

    // 2. Curve confirmation loop first (lightweight getAccount polls before heavy getSignaturesForAddress).
    let poll_interval = Duration::from_millis(900);
    let max_polls = ((volume_confirm_secs.max(3) * 1000) / 900).max(4);
    let mut current_real_sol: u64 = initial_real_sol;
    let mut peak_real_sol: u64 = initial_real_sol;
    let mut prev_real_sol: u64 = initial_real_sol;
    let mut poll_0_real_sol: Option<u64> = None;
    let mut rising_ticks: u32 = 0;
    let mut qualified_ticks: u32 = 0;
    let mut confirmed = false;

    tracing::info!(
        mint = %signal.mint,
        pool = %signal.pool_address,
        initial_sol = initial_real_sol as f64 / 1e9,
        min_sol = min_pool_sol,
        max_sol = max_pool_sol,
        min_inflow = min_inflow_sol,
        window_secs = volume_confirm_secs,
        "Running second-wave organic inflow & anti-dump confirmation"
    );

    for i in 0..max_polls {
        tokio::time::sleep(poll_interval).await;
        if let Ok(acc) = rpc.get_account(pool_pk).await {
            if let Ok(curve) = PumpFunBondingCurve::from_account_data(&acc.data, pool_pk, mint_pk) {
                if curve.complete {
                    return CheckResult {
                        passed: false,
                        reason: Some("Pool completed during confirmation window".to_string()),
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                }
                if !is_standard_sol_curve(&curve) {
                    return CheckResult {
                        passed: false,
                        reason: Some("Non-standard virtual SOL base".to_string()),
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                }

                current_real_sol = curve.real_sol_reserves;
                if i == 0 {
                    poll_0_real_sol = Some(current_real_sol);
                }

                if current_real_sol > max_lamports {
                    return CheckResult {
                        passed: false,
                        reason: Some(format!(
                            "Overheated pool / whale bundle: {:.2} SOL > max {:.2} SOL",
                            current_real_sol as f64 / 1e9,
                            max_pool_sol
                        )),
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                }

                // Two-tier anti-dump check: allow controlled shakeouts at low SOL, reject heavy dumps or >11% dumps at >= 2x min_pool_sol
                if peak_real_sol > 0
                    && (current_real_sol as f64)
                        < (peak_real_sol as f64) * (1.0 - max_confirm_drop_pct)
                {
                    return CheckResult {
                        passed: false,
                        reason: Some(format!(
                            "Early dump detected during confirmation window: {:.2} SOL < {:.2} SOL peak (-{:.1}%)",
                            current_real_sol as f64 / 1e9,
                            peak_real_sol as f64 / 1e9,
                            (1.0 - (current_real_sol as f64 / peak_real_sol as f64)) * 100.0
                        )),
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                }
                if peak_real_sol >= min_lamports.saturating_mul(2)
                    && (current_real_sol as f64) < (peak_real_sol as f64) * 0.89
                {
                    return CheckResult {
                        passed: false,
                        reason: Some(format!(
                            "Early high-liquidity dump detected during confirmation window: {:.2} SOL < {:.2} SOL peak (-{:.1}%)",
                            current_real_sol as f64 / 1e9,
                            peak_real_sol as f64 / 1e9,
                            (1.0 - (current_real_sol as f64 / peak_real_sol as f64)) * 100.0
                        )),
                        duration_ms: start.elapsed().as_millis() as u64,
                    };
                }

                if current_real_sol > prev_real_sol.saturating_add(40_000_000) {
                    rising_ticks += 1;
                }

                peak_real_sol = peak_real_sol.max(current_real_sol);
                let total_inflow_lamports = current_real_sol.saturating_sub(initial_real_sol);
                let post_p0_inflow_lamports = poll_0_real_sol
                    .map(|p0| current_real_sol.saturating_sub(p0))
                    .unwrap_or(0);
                let has_second_wave =
                    i >= 2 && post_p0_inflow_lamports >= (min_inflow_lamports / 2).max(200_000_000);
                let meets_threshold =
                    current_real_sol >= min_lamports && total_inflow_lamports >= min_inflow_lamports;
                let non_decreasing =
                    current_real_sol >= prev_real_sol.saturating_mul(99) / 100;
                let near_peak = current_real_sol >= peak_real_sol.saturating_mul(96) / 100;

                if meets_threshold && non_decreasing && near_peak {
                    qualified_ticks += 1;
                } else if !meets_threshold
                    || !near_peak
                    || current_real_sol < prev_real_sol.saturating_mul(97) / 100
                {
                    qualified_ticks = 0;
                }
                prev_real_sol = current_real_sol;

                let min_poll_idx =
                    if initial_real_sol >= 11_000_000_000 || total_inflow_lamports > 5_000_000_000 {
                        3
                    } else {
                        2
                    };
                if i >= min_poll_idx
                    && qualified_ticks >= 2
                    && rising_ticks >= 2
                    && has_second_wave
                {
                    tracing::info!(
                        mint = %signal.mint,
                        initial_sol = initial_real_sol as f64 / 1e9,
                        poll0_sol = poll_0_real_sol.unwrap_or(0) as f64 / 1e9,
                        real_sol = current_real_sol as f64 / 1e9,
                        inflow_sol = total_inflow_lamports as f64 / 1e9,
                        post_p0_inflow_sol = post_p0_inflow_lamports as f64 / 1e9,
                        poll_idx = i,
                        qualified_ticks = qualified_ticks,
                        rising_ticks = rising_ticks,
                        "Curve second-wave organic inflow & anti-dump confirmed — running tx/freshness check"
                    );
                    confirmed = true;
                    break;
                }

                // Credit-saving early exit: if after 3 polls (~2.7s) a dead pool is still < 1.5 SOL
                // with < 0.25 SOL inflow while we require >= 5 SOL, stop polling early.
                if i == 2
                    && min_lamports >= 5_000_000_000
                    && current_real_sol < 1_500_000_000
                    && total_inflow_lamports < 250_000_000
                {
                    break;
                }
            }
        }
    }

    if !confirmed {
        let real_sol_sol = current_real_sol as f64 / 1_000_000_000.0;
        let inflow_sol_actual =
            current_real_sol.saturating_sub(initial_real_sol) as f64 / 1_000_000_000.0;
        tracing::warn!(
            mint = %signal.mint,
            pool = %signal.pool_address,
            real_sol = real_sol_sol,
            inflow_sol = inflow_sol_actual,
            min_required = min_pool_sol,
            min_inflow = min_inflow_sol,
            "Rejecting pool due to insufficient real liquidity or organic inflow"
        );

        return CheckResult {
            passed: false,
            reason: Some(format!(
                "Insufficient organic inflow/liquidity: real={:.2} SOL (min {:.2}), inflow={:.2} SOL (min {:.2})",
                real_sol_sol, min_pool_sol, inflow_sol_actual, min_inflow_sol
            )),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // Complete deferred deployer reputation check for launches that started < 2 SOL and surged.
    if let Some(creator) = deferred_creator {
        if let Err(reason) =
            verify_deployer_reputation(rpc, &creator, max_deployer_txs, 6 * 3600).await
        {
            tracing::warn!(mint = %signal.mint, reason = %reason, "Rejecting serial deployer (deferred)");
            return CheckResult {
                passed: false,
                reason: Some(reason),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    }

    // 3. Pool age / multi-slot transaction check AFTER organic SOL inflow is confirmed
    //    (saves getSignaturesForAddress RPC quota and allows 2 retries of 600ms if still indexing).
    let required_buyers = if current_real_sol >= 12_000_000_000 {
        min_unique_buyers.max(5)
    } else {
        min_unique_buyers
    };
    for attempt in 0..3_u8 {
        match verify_pool_freshness_and_transactions(
            rpc,
            pool_pk,
            required_buyers,
            max_pool_age_secs,
        )
        .await
        {
            Ok(()) => break,
            Err(reason) if reason.starts_with("Not enough transactions") && attempt < 2 => {
                tokio::time::sleep(Duration::from_millis(600)).await;
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

    // Final curve check after freshness/signature call to make sure pool didn't dump or overheat meanwhile
    let mut final_real_sol = current_real_sol;
    if let Ok(acc) = rpc.get_account(pool_pk).await {
        if let Ok(curve) = PumpFunBondingCurve::from_account_data(&acc.data, pool_pk, mint_pk) {
            if curve.complete || !is_standard_sol_curve(&curve) {
                return CheckResult {
                    passed: false,
                    reason: Some("Pool completed or became non-standard after freshness check".to_string()),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
            if curve.real_sol_reserves > max_lamports {
                return CheckResult {
                    passed: false,
                    reason: Some(format!(
                        "Overheated pool after freshness check: {:.2} SOL > max {:.2} SOL",
                        curve.real_sol_reserves as f64 / 1e9,
                        max_pool_sol
                    )),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
            if curve.real_sol_reserves < current_real_sol.saturating_mul(91) / 100 {
                return CheckResult {
                    passed: false,
                    reason: Some(format!(
                        "Pool dumped after freshness check: {:.2} < {:.2} SOL",
                        curve.real_sol_reserves as f64 / 1e9,
                        current_real_sol as f64 / 1e9
                    )),
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
            final_real_sol = curve.real_sol_reserves;
        }
    }

    record_confirmed_pool_sol(&signal.pool_address, final_real_sol);
    tracing::info!(
        mint = %signal.mint,
        initial_sol = initial_real_sol as f64 / 1e9,
        final_real_sol = final_real_sol as f64 / 1e9,
        "Organic inflow & freshness confirmed"
    );
    CheckResult {
        passed: true,
        reason: None,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

static DEPLOYER_REPUTATION_CACHE: std::sync::OnceLock<dashmap::DashMap<Pubkey, (Option<String>, Instant)>> =
    std::sync::OnceLock::new();

fn check_cached_serial_deployer(creator: &Pubkey) -> Option<String> {
    let cache = DEPLOYER_REPUTATION_CACHE.get()?;
    let entry = cache.get(creator)?;
    let (reason_opt, cached_at) = entry.value();
    if let Some(reason) = reason_opt {
        if cached_at.elapsed() < Duration::from_secs(3600) {
            return Some(reason.clone());
        }
    }
    None
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
    let cache = DEPLOYER_REPUTATION_CACHE.get_or_init(dashmap::DashMap::new);
    if let Some(entry) = cache.get(creator) {
        let (reason_opt, cached_at) = entry.value();
        match reason_opt {
            Some(reason) if cached_at.elapsed() < Duration::from_secs(3600) => {
                return Err(reason.clone());
            }
            None if cached_at.elapsed() < Duration::from_secs(600) => {
                return Ok(());
            }
            _ => {}
        }
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
                let reason = format!(
                    "Serial deployer: creator {} has {} txs in last {}s (max {})",
                    creator, recent, window_secs, max_recent_txs
                );
                cache.insert(*creator, (Some(reason.clone()), Instant::now()));
                return Err(reason);
            }
            cache.insert(*creator, (None, Instant::now()));
            Ok(())
        }
        Err(e) => {
            tracing::warn!(creator = %creator, error = %e, "Deployer history fetch failed - not blocking");
            Ok(())
        }
    }
}

/// Verify that the pool is fresh (not an old/zombie token), active across multiple slots, and has sufficient buyers
async fn verify_pool_freshness_and_transactions(
    rpc: &Arc<RpcClient>,
    pool_pk: &Pubkey,
    min_txs: usize,
    max_pool_age_secs: u64,
) -> Result<(), String> {
    use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
    let make_cfg = || GetConfirmedSignaturesForAddress2Config {
        before: None,
        until: None,
        limit: Some(100),
        commitment: None,
    };
    let sigs_res = match rpc.get_signatures_for_address_with_config(pool_pk, make_cfg()).await {
        Ok(s) => Ok(s),
        Err(_) => {
            tokio::time::sleep(Duration::from_millis(350)).await;
            rpc.get_signatures_for_address_with_config(pool_pk, make_cfg()).await
        }
    };
    match sigs_res {
        Ok(sigs) => {
            let ok_sigs: Vec<_> = sigs.iter().filter(|s| s.err.is_none()).collect();
            if ok_sigs.len() < min_txs {
                return Err(format!(
                    "Not enough transactions: {} < {} required",
                    ok_sigs.len(),
                    min_txs
                ));
            }

            if sigs.len() >= 10 && ok_sigs.len() * 2 < sigs.len() {
                return Err(format!(
                    "Bot spam / high failed-tx ratio on pool: {} ok of {} total txs",
                    ok_sigs.len(),
                    sigs.len()
                ));
            }

            if !ok_sigs.is_empty() {
                let min_slot = ok_sigs.iter().map(|s| s.slot).min().unwrap_or(0);
                let max_slot = ok_sigs.iter().map(|s| s.slot).max().unwrap_or(0);
                if max_slot <= min_slot {
                    return Err(format!(
                        "Not enough transactions across multiple slots: all {} txs in slot {}",
                        ok_sigs.len(),
                        min_slot
                    ));
                }
            }

            let now_ts = chrono::Utc::now().timestamp();

            // Earliest signature in this batch indicates pool creation time
            if let Some(earliest_sig) = sigs.last() {
                if let Some(bt) = earliest_sig.block_time {
                    let age = now_ts.saturating_sub(bt);
                    if age > max_pool_age_secs as i64 {
                        return Err(format!(
                            "Pool is too old: created/active {}s ago (max allowed: {}s)",
                            age, max_pool_age_secs
                        ));
                    }
                }
            }

            // Latest signature indicates recent trading activity
            if let Some(latest_sig) = ok_sigs.first() {
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
        Err(e) => Err(format!("Failed to fetch signatures for pool: {}", e)),
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
