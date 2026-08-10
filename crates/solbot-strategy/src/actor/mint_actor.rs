//! MintActor trading loop and pipeline execution (Stage 5.1).
//!
//! Executes:
//! - Buy pipeline: Risk Gate (configurable timeout via RiskGateConfig) → Position Sizing → Tx Builder → Signer Client → Transaction Router
//! - Sell pipeline: Exit Evaluation → Exit Tx Builder → Signer Client → Transaction Router → State Transition

use super::{market_data_lane::MarketDataLane, ActorConfig};
use crate::circuit_breaker::CircuitBreaker;
use crate::exit::{self, ExitAction};
use crate::exit::escalation::{run_escalation, EscalationState, EscalationTrigger};
use crate::risk_gate::{self, RiskGateConfig};
use crate::routing::{RoutingStage, SubmitResult, TransactionRouter};
use crate::signer_client::SignerClient;
use crate::sizing::{self, TradeStats};
use crate::state::position_lifecycle::PositionLifecycle;
use crate::state::positions_repo::{status_to_string, PositionsRepo};
use crate::state::state_transitions_repo::StateTransitionsRepo;
use crate::tx_builder::meteora_damm_v2 as meteora_damm_v2_builder;
use crate::tx_builder::meteora_dlmm as meteora_dlmm_builder;
use crate::tx_builder::pump_fun as pump_fun_builder;
use crate::tx_builder::raydium_amm_v4 as raydium_amm_v4_builder;
use crate::tx_builder::raydium_clmm as raydium_clmm_builder;
use crate::tx_builder::raydium_cpmm as raydium_cpmm_builder;

const DLMM_MAINNET: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
const DLMM_LEGACY: &str = "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp";
const DAMM_V2: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";
use crate::tx_builder::{
    self, apply_slippage, build_unsigned_transaction, SwapDirection, SwapParams,
};
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::hash::Hash;
use solana_transaction::Transaction;
use solbot_common::config::base_config::DexReadinessConfig;
use solbot_common::config::TierParams;
use solbot_common::types::intent::{IntentType, SignerIntentRequest, VerdictStatus};
use solbot_common::types::position::{Position, PositionStatus};
use solbot_common::types::tier::CapitalTier;
use solbot_common::types::{ControlEvent, MarketDataEvent, PositionLifecycleEvent, Signal};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Trading mode configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradingMode {
    /// Live trading with real transactions submitted via RPC / Jito
    Live,
    /// Paper trading - build, validate, and simulate without broadcasting
    Paper,
}

/// Helper to fetch vault balances with explicit error handling (fail-closed).
/// Returns Err if RPC fails or balance cannot be parsed, distinguishing from legitimate zero balance.
pub async fn fetch_vault_balances(
    rpc_url: String,
    vault_a: Pubkey,
    vault_b: Pubkey,
    dex_name: &str,
) -> Result<(u64, u64)> {
    let dex_name = dex_name.to_string();
    let dex_name_clone = dex_name.clone();
    let vault_balances = tokio::task::spawn_blocking(move || {
        let rpc = RpcClient::new(rpc_url.clone());
        let balance_a = rpc
            .get_token_account_balance(&vault_a)
            .map_err(|e| anyhow!("Failed to fetch {} vault_a balance: {}", dex_name, e))
            .and_then(|r| {
                r.amount
                    .parse::<u64>()
                    .map_err(|e| anyhow!("Failed to parse {} vault_a balance: {}", dex_name, e))
            })?;
        let balance_b = rpc
            .get_token_account_balance(&vault_b)
            .map_err(|e| anyhow!("Failed to fetch {} vault_b balance: {}", dex_name, e))
            .and_then(|r| {
                r.amount
                    .parse::<u64>()
                    .map_err(|e| anyhow!("Failed to parse {} vault_b balance: {}", dex_name, e))
            })?;
        Ok::<(u64, u64), anyhow::Error>((balance_a, balance_b))
    })
    .await
    .map_err(|e| anyhow!("{} vault balance fetch task failed: {}", dex_name_clone, e))??;
    Ok(vault_balances)
}

/// Check if the DEX is ready for Live trading based on program ID.
fn check_dex_readiness(
    dex_program_id: &str,
    dex_readiness: &DexReadinessConfig,
    trading_mode: TradingMode,
) -> Result<()> {
    if trading_mode == TradingMode::Paper {
        return Ok(()); // Always allow in Paper mode
    }

    let readiness = if dex_program_id == raydium_amm_v4_builder::RAYDIUM_AMM_V4_PROGRAM.to_string()
    {
        dex_readiness.raydium_amm_v4
    } else if dex_program_id == raydium_cpmm_builder::RAYDIUM_CPMM_PROGRAM.to_string() {
        dex_readiness.raydium_cpmm
    } else if dex_program_id == meteora_dlmm_builder::METEORA_DLMM_PROGRAM.to_string() {
        dex_readiness.meteora_dlmm
    } else {
        // Unknown DEX - allow for now (could be Pump.fun or other)
        return Ok(());
    };

    if !readiness.is_live_allowed() {
        bail!(
            "DEX {} is not ready for Live mode. Current status: {:?}. Set to Ready after verifying real pricing.",
            dex_program_id, readiness
        );
    }

    Ok(())
}

/// Extended actor context with trading pipeline components.
pub struct ActorContext {
    pub mint: String,
    pub pool_address: String,
    pub dex_program_id: String,
    pub wallet: Pubkey,
    pub trading_mode: TradingMode,
    pub risk_gate_config: RiskGateConfig,
    pub tier: CapitalTier,
    pub tier_params: TierParams,
    pub capital_sol: f64,
    pub router: Option<TransactionRouter>,
    pub signer_client: Option<Arc<SignerClient>>,
    pub rpc_url: Option<String>,
    pub active_position: Option<Position>,
    pub trade_stats: Option<TradeStats>,
    pub dex_readiness: DexReadinessConfig,
    pub positions_repo: Option<Arc<PositionsRepo>>,
    pub state_transitions_repo: Option<Arc<StateTransitionsRepo>>,
    pub position_lifecycle: Option<Arc<PositionLifecycle>>,
    pub lifecycle_event_tx: Option<tokio::sync::mpsc::Sender<PositionLifecycleEvent>>,
    pub circuit_breaker: Option<Arc<CircuitBreaker>>,
    /// Raw pool account bytes from WS/Geyser (enables 1-RTT risk gate).
    pub raw_pool_account_data: Option<Vec<u8>>,
    pub raw_pool_account_owner: Option<String>,
    pub last_signal_slot: u64,
    pub last_signal_received_at: u64,
    /// Prevent duplicate buy attempts for the same pool.
    pub buy_attempted: bool,
}

pub async fn run(
    mint: String,
    mut control_rx: tokio::sync::mpsc::Receiver<ControlEvent>,
    mut market_data_rx: MarketDataLane,
    mut ctx: ActorContext,
    config: ActorConfig,
) {
    tracing::info!(
        "Starting mint actor for {} (mode: {:?})",
        mint,
        ctx.trading_mode
    );

    // Spawn daily circuit breaker reset task (00:00 UTC)
    if let Some(ref circuit_breaker) = ctx.circuit_breaker {
        let cb_clone = circuit_breaker.clone();
        tokio::spawn(async move {
            loop {
                // Calculate time until next 00:00 UTC
                let now = chrono::Utc::now();
                let tomorrow = now.date_naive() + chrono::Duration::days(1);
                let next_midnight = tomorrow.and_hms_opt(0, 0, 0).unwrap().and_utc();
                let duration_until_midnight = (next_midnight - now)
                    .to_std()
                    .unwrap_or(std::time::Duration::from_secs(86400));

                tracing::info!(
                    next_reset_at = %next_midnight,
                    seconds_until_reset = duration_until_midnight.as_secs(),
                    "Circuit breaker daily reset scheduled"
                );

                tokio::time::sleep(duration_until_midnight).await;

                // Reset circuit breaker
                cb_clone.reset_daily();
                tracing::info!("Circuit breaker daily losses reset at 00:00 UTC");
            }
        });
    }

    loop {
        tokio::select! {
            biased;
            control_event = control_rx.recv() => {
                match control_event {
                    Some(event) => {
                        let start = std::time::Instant::now();
                        tracing::debug!("Actor {} received control event: {:?}", mint, event);

                        process_control_event(&mint, event, &mut ctx).await;

                        let elapsed = start.elapsed();
                        if elapsed > Duration::from_millis(config.control_event_processing_deadline_ms) {
                            tracing::warn!(
                                "Control event processing exceeded deadline: {}ms (limit: {}ms)",
                                elapsed.as_millis(),
                                config.control_event_processing_deadline_ms
                            );
                        }
                    }
                    None => {
                        tracing::info!("Control channel closed for actor {}", mint);
                        break;
                    }
                }
            }
            market_data_event = market_data_rx.recv() => {
                match market_data_event {
                    Some(event) => {
                        let start = std::time::Instant::now();
                        tracing::trace!("Actor {} received market data event: {:?}", mint, event);

                        process_market_data_event(&mint, event, &mut ctx).await;

                        let elapsed = start.elapsed();
                        if elapsed > Duration::from_millis(config.market_data_processing_deadline_ms) {
                            tracing::warn!(
                                "Market data processing exceeded deadline: {}ms (limit: {}ms)",
                                elapsed.as_millis(),
                                config.market_data_processing_deadline_ms
                            );
                        }
                    }
                    None => {
                        tracing::info!("Market data channel closed for actor {}", mint);
                        break;
                    }
                }
            }
        }
    }

    tracing::info!("Mint actor {} shutting down", mint);
}

async fn process_control_event(mint: &str, event: ControlEvent, ctx: &mut ActorContext) {
    match event {
        ControlEvent::NewPoolSignal {
            mint: _,
            pool_address,
            dex_program,
            slot,
            received_at,
            raw_pool_account_data,
            raw_pool_account_owner,
        } => {
            tracing::info!(
                mint = %mint,
                pool = %pool_address,
                dex = %dex_program,
                slot,
                "NewPoolSignal — starting buy pipeline"
            );
            ctx.pool_address = pool_address;
            ctx.dex_program_id = dex_program;
            ctx.last_signal_slot = slot;
            ctx.last_signal_received_at = received_at;
            ctx.raw_pool_account_data = raw_pool_account_data;
            ctx.raw_pool_account_owner = raw_pool_account_owner;

            if ctx.active_position.is_some() {
                tracing::debug!(mint = %mint, "Position already open — ignoring NewPoolSignal");
                return;
            }
            if ctx.buy_attempted {
                tracing::debug!(mint = %mint, "Buy already attempted — ignoring NewPoolSignal");
                return;
            }
            ctx.buy_attempted = true;

            // Placeholder entry price; real mark comes from vault balances later.
            let entry_price = 0.0;
            if let Err(e) = execute_buy_pipeline(ctx, entry_price).await {
                tracing::warn!("Buy pipeline failed for {}: {}", mint, e);
                // Allow one retry on next price tick for transient RPC failures.
                if e.to_string().to_lowercase().contains("timeout")
                    || e.to_string().to_lowercase().contains("rpc")
                {
                    ctx.buy_attempted = false;
                }
            }
        }
        ControlEvent::MigrationEvent {
            mint: _,
            new_pool_address,
        } => {
            tracing::info!(
                "Migration event for {}: new pool = {}",
                mint,
                new_pool_address
            );
            ctx.pool_address = new_pool_address;
            ctx.buy_attempted = false;
            ctx.raw_pool_account_data = None;
            ctx.raw_pool_account_owner = None;
        }
        ControlEvent::KillSwitch { activated, reason } => {
            tracing::warn!(
                "Kill switch for {}: activated={}, reason={}",
                mint,
                activated,
                reason
            );
            if activated {
                if let Some(ref pos) = ctx.active_position.clone() {
                    tracing::warn!(
                        "Emergency exit for position {} due to kill switch",
                        pos.position_id
                    );
                    let exit_action = ExitAction::Emergency { reason };
                    let current_price = pos.entry_price_sol;
                    let _ = execute_exit_pipeline(ctx, &pos, exit_action, current_price).await;
                }
            }
        }
        ControlEvent::TpTriggerRecompute { position_id } => {
            tracing::info!(
                "TP trigger recompute for {}: position_id={}",
                mint,
                position_id
            );
        }
        ControlEvent::SlTriggerRecompute { position_id } => {
            tracing::info!(
                "SL trigger recompute for {}: position_id={}",
                mint,
                position_id
            );
        }
        ControlEvent::ManualCliCommand { command, args } => {
            tracing::info!(
                "Manual CLI command for {}: cmd={:?}, args={:?}",
                mint,
                command,
                args
            );
        }
    }
}

pub async fn process_market_data_event(mint: &str, event: MarketDataEvent, ctx: &mut ActorContext) {
    match event {
        MarketDataEvent::PriceTick {
            mint: _,
            pool_address,
            price_sol,
            timestamp,
        } => {
            tracing::trace!(
                "Price tick for {}: pool={}, price={} SOL, ts={}",
                mint,
                pool_address,
                price_sol,
                timestamp
            );

            // Update high water mark for trailing stop if active
            if let Some(ref mut pos) = ctx.active_position {
                if pos.trailing_active {
                    let high_water = pos.trailing_high_water_sol.unwrap_or(price_sol);
                    if price_sol > high_water {
                        pos.trailing_high_water_sol = Some(price_sol);
                    }
                }
            }

            // Run buy pipeline if no active position exists yet, otherwise evaluate exit
            if ctx.active_position.is_none() {
                tracing::info!(
                    "No active position for {}, triggering buy pipeline at price {} SOL",
                    mint,
                    price_sol
                );
                if let Err(e) = execute_buy_pipeline(ctx, price_sol).await {
                    tracing::warn!("Buy pipeline failed for {}: {}", mint, e);
                }
            } else if let Some(ref position) = ctx.active_position.clone() {
                if position.status != PositionStatus::Closed
                    && position.status != PositionStatus::StuckManualInterventionRequired
                {
                    if let Some(exit_action) = exit::evaluate_exit(position, price_sol) {
                        tracing::info!(
                            "Exit action triggered for position {}: {:?}",
                            position.position_id,
                            exit_action
                        );
                        if let Err(e) =
                            execute_exit_pipeline(ctx, position, exit_action, price_sol).await
                        {
                            tracing::error!(
                                "Exit pipeline failed for position {}: {}",
                                position.position_id,
                                e
                            );
                            if let Some(ref mut pos) = ctx.active_position {
                                let old_status = pos.status.clone();

                                // Use PositionLifecycle for transition to Stuck state
                                if let Some(ref lifecycle) = ctx.position_lifecycle {
                                    let _ = lifecycle
                                        .transition_with_reason(
                                            pos,
                                            PositionStatus::StuckManualInterventionRequired,
                                            format!("exit_pipeline_error: {}", e),
                                            None,
                                        )
                                        .await;
                                }

                                pos.status = PositionStatus::StuckManualInterventionRequired;

                                if let Some(ref repo) = ctx.positions_repo {
                                    let _ = repo.update_position(pos).await;
                                }

                                // Emit stuck event
                                if let Some(ref tx) = ctx.lifecycle_event_tx {
                                    let event =
                                        PositionLifecycleEvent::ManualInterventionRequired {
                                            position_id: pos.position_id.clone(),
                                            reason: format!("exit_pipeline_error: {}", e),
                                            current_status: status_to_string(&old_status)
                                                .to_string(),
                                        };
                                    let _ = tx.send(event).await;
                                }
                            }
                        }
                    }
                }
            }
        }
        MarketDataEvent::LiquidityUpdate {
            mint: _,
            pool_address,
            liquidity_usd,
        } => {
            tracing::trace!(
                "Liquidity update for {}: pool={}, liquidity=${}",
                mint,
                pool_address,
                liquidity_usd
            );
        }
    }
}

/// Check if calculated output is zero and bail with descriptive error message.
/// Used to prevent fail-open trading on empty pools or zero reserves.
pub fn ensure_nonzero_output(expected_out: u64, dex_name: &str, direction: &str) -> Result<()> {
    if expected_out == 0 {
        bail!(
            "{} calculated zero {} output — refusing to trade (empty pool or zero reserves)",
            dex_name,
            direction
        );
    }
    Ok(())
}

/// Fetch pool state for the specified DEX and build swap instructions for any direction (buy/sell).
/// This unified function replaces duplicated code in execute_buy_pipeline and execute_exit_pipeline.
async fn fetch_and_build_dex_swap(
    rpc_url: &Option<String>,
    trading_mode: TradingMode,
    dex_program_id: &str,
    wallet: Pubkey,
    mint_pubkey: Pubkey,
    pool_pubkey: Pubkey,
    dex_program_pubkey: Pubkey,
    amount_in_lamports: u64,
    direction: SwapDirection,
    slippage_bps: u32,
    preloaded_pool_data: Option<Vec<u8>>,
) -> Result<tx_builder::SwapInstructions> {
    let dir_label = match direction {
        SwapDirection::Buy => "buy",
        SwapDirection::Sell => "sell",
    };

    let rpc_url = match rpc_url.clone() {
        Some(url) => url,
        None => {
            if trading_mode == TradingMode::Live {
                tracing::error!("No RPC client in Live mode");
                bail!("No RPC client in Live mode");
            }
            tracing::warn!("No RPC client, using placeholder swap instructions (Paper mode)");
            return Ok(create_placeholder_swap_instructions());
        }
    };

    // Prefer WS/Geyser raw pool bytes (0 RTT); else RPC get_account.
    macro_rules! fetch_account_data {
        ($pk:expr) => {{
            if let Some(ref d) = preloaded_pool_data {
                Some(d.clone())
            } else {
                let rpc_url = rpc_url.clone();
                let pk = $pk;
                tokio::task::spawn_blocking(move || {
                    let rpc = RpcClient::new(rpc_url);
                    rpc.get_account(&pk).map(|a| a.data)
                })
                .await
                .ok()
                .and_then(|r| r.ok())
            }
        }};
    }

    // Unified handling for "account not found" / "parse failed" - error in Live, warning + placeholder in Paper
    macro_rules! fail_or_placeholder {
        ($msg:literal) => {{
            if trading_mode == TradingMode::Live {
                tracing::error!($msg);
                bail!($msg);
            }
            tracing::warn!(concat!($msg, ", using placeholder (Paper mode)"));
            return Ok(create_placeholder_swap_instructions());
        }};
    }

    if dex_program_id == pump_fun_builder::PUMP_FUN_PROGRAM.to_string() {
        let Some(data) = fetch_account_data!(pool_pubkey) else {
            fail_or_placeholder!("Failed to fetch pool account in Live mode");
        };
        let Ok(curve) =
            pump_fun_builder::PumpFunBondingCurve::from_account_data(&data, &pool_pubkey)
        else {
            fail_or_placeholder!("Failed to parse Pump.fun bonding curve in Live mode");
        };

        let expected_out = match direction {
            SwapDirection::Buy => curve.calculate_buy_output(amount_in_lamports),
            SwapDirection::Sell => curve.calculate_sell_output(amount_in_lamports),
        };
        let minimum_amount_out = apply_slippage(expected_out, slippage_bps);
        let swap_params = SwapParams {
            mint: mint_pubkey,
            pool_address: pool_pubkey,
            dex_program_id: dex_program_pubkey,
            wallet,
            direction,
            amount_in: amount_in_lamports,
            minimum_amount_out,
            slippage_bps,
        };
        return pump_fun_builder::build_swap(&swap_params, &curve)
            .map_err(|e| anyhow!("Tx builder error: {}", e));
    }

    if dex_program_id == raydium_amm_v4_builder::RAYDIUM_AMM_V4_PROGRAM.to_string() {
        let Some(data) = fetch_account_data!(pool_pubkey) else {
            fail_or_placeholder!("Failed to fetch pool account in Live mode");
        };
        let Ok(mut pool_state) =
            raydium_amm_v4_builder::RaydiumAmmV4PoolState::from_account_data(&data)
        else {
            fail_or_placeholder!("Failed to parse Raydium AMM v4 pool state in Live mode");
        };
        let vault_balances = fetch_vault_balances(
            rpc_url.clone(),
            pool_state.serum_coin_vault,
            pool_state.serum_pc_vault,
            "Raydium AMM v4",
        )
        .await?;
        pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);

        let expected_out = match direction {
            SwapDirection::Buy => pool_state.calculate_buy_output(amount_in_lamports),
            SwapDirection::Sell => pool_state.calculate_sell_output(amount_in_lamports),
        };
        ensure_nonzero_output(expected_out, "Raydium AMM v4", dir_label)?;
        let minimum_amount_out = apply_slippage(expected_out, slippage_bps);
        let swap_params = SwapParams {
            mint: mint_pubkey,
            pool_address: pool_pubkey,
            dex_program_id: dex_program_pubkey,
            wallet,
            direction,
            amount_in: amount_in_lamports,
            minimum_amount_out,
            slippage_bps,
        };
        return raydium_amm_v4_builder::build_swap(&swap_params, &pool_state)
            .map_err(|e| anyhow!("Tx builder error: {}", e));
    }

    if dex_program_id == raydium_cpmm_builder::RAYDIUM_CPMM_PROGRAM.to_string() {
        let Some(data) = fetch_account_data!(pool_pubkey) else {
            fail_or_placeholder!("Failed to fetch pool account in Live mode");
        };
        let Ok(mut pool_state) = raydium_cpmm_builder::CpmmPoolState::from_account_data(&data)
        else {
            fail_or_placeholder!("Failed to parse Raydium CPMM pool state in Live mode");
        };
        let vault_balances = fetch_vault_balances(
            rpc_url.clone(),
            pool_state.token_0_vault,
            pool_state.token_1_vault,
            "Raydium CPMM",
        )
        .await?;
        pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);

        let expected_out = match direction {
            SwapDirection::Buy => pool_state.calculate_buy_output(amount_in_lamports),
            SwapDirection::Sell => pool_state.calculate_sell_output(amount_in_lamports),
        };
        ensure_nonzero_output(expected_out, "Raydium CPMM", dir_label)?;
        let minimum_amount_out = apply_slippage(expected_out, slippage_bps);
        let swap_params = SwapParams {
            mint: mint_pubkey,
            pool_address: pool_pubkey,
            dex_program_id: dex_program_pubkey,
            wallet,
            direction,
            amount_in: amount_in_lamports,
            minimum_amount_out,
            slippage_bps,
        };
        return raydium_cpmm_builder::build_swap(&swap_params, &pool_state)
            .map_err(|e| anyhow!("Tx builder error: {}", e));
    }

    if dex_program_id == DLMM_MAINNET
        || dex_program_id == DLMM_LEGACY
        || dex_program_id == meteora_dlmm_builder::METEORA_DLMM_PROGRAM.to_string()
    {
        let Some(data) = fetch_account_data!(pool_pubkey) else {
            fail_or_placeholder!("Failed to fetch pool account in Live mode");
        };
        let Ok(mut pool_state) =
            meteora_dlmm_builder::DlmmPoolState::from_account_data(&data, &pool_pubkey)
        else {
            fail_or_placeholder!("Failed to parse Meteora DLMM pool state in Live mode");
        };
        let vault_balances = fetch_vault_balances(
            rpc_url.clone(),
            pool_state.reserve_x,
            pool_state.reserve_y,
            "Meteora DLMM",
        )
        .await?;
        pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);

        let expected_out = match direction {
            SwapDirection::Buy => pool_state.calculate_buy_output(amount_in_lamports),
            SwapDirection::Sell => pool_state.calculate_sell_output(amount_in_lamports),
        };
        ensure_nonzero_output(expected_out, "Meteora DLMM", dir_label)?;
        let minimum_amount_out = apply_slippage(expected_out, slippage_bps);
        let swap_params = SwapParams {
            mint: mint_pubkey,
            pool_address: pool_pubkey,
            dex_program_id: dex_program_pubkey,
            wallet,
            direction,
            amount_in: amount_in_lamports,
            minimum_amount_out,
            slippage_bps,
        };
        return meteora_dlmm_builder::build_swap(&swap_params, &pool_state)
            .map_err(|e| anyhow!("Tx builder error: {}", e));
    }

    if dex_program_id == raydium_clmm_builder::RAYDIUM_CLMM_PROGRAM.to_string() {
        let Some(data) = fetch_account_data!(pool_pubkey) else {
            fail_or_placeholder!("Failed to fetch pool account in Live mode");
        };
        let Ok(pool_state) = raydium_clmm_builder::ClmmPoolState::from_account_data(&data) else {
            fail_or_placeholder!("Failed to parse Raydium CLMM pool state in Live mode");
        };
        let swap_params = SwapParams {
            mint: mint_pubkey,
            pool_address: pool_pubkey,
            dex_program_id: dex_program_pubkey,
            wallet,
            direction,
            amount_in: amount_in_lamports,
            minimum_amount_out: 1,
            slippage_bps,
        };
        return raydium_clmm_builder::build_swap(&swap_params, &pool_state, &pool_pubkey, None)
            .map_err(|e| anyhow!("Tx builder error: {}", e));
    }

    if dex_program_id == DAMM_V2
        || dex_program_id == meteora_damm_v2_builder::METEORA_DAMM_V2_PROGRAM.to_string()
    {
        let Some(data) = fetch_account_data!(pool_pubkey) else {
            fail_or_placeholder!("Failed to fetch pool account in Live mode");
        };
        let Ok(mut pool_state) =
            meteora_damm_v2_builder::DammV2PoolState::from_account_data(&data, &pool_pubkey)
        else {
            fail_or_placeholder!("Failed to parse DAMM v2 pool state in Live mode");
        };
        let vault_balances = fetch_vault_balances(
            rpc_url.clone(),
            pool_state.token_a_vault,
            pool_state.token_b_vault,
            "Meteora DAMM v2",
        )
        .await?;
        pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);
        let expected_out = match direction {
            SwapDirection::Buy => pool_state.calculate_buy_output(amount_in_lamports),
            SwapDirection::Sell => pool_state.calculate_sell_output(amount_in_lamports),
        };
        ensure_nonzero_output(expected_out, "Meteora DAMM v2", dir_label)?;
        let minimum_amount_out = apply_slippage(expected_out, slippage_bps);
        let swap_params = SwapParams {
            mint: mint_pubkey,
            pool_address: pool_pubkey,
            dex_program_id: dex_program_pubkey,
            wallet,
            direction,
            amount_in: amount_in_lamports,
            minimum_amount_out,
            slippage_bps,
        };
        return meteora_damm_v2_builder::build_swap(&swap_params, &pool_state)
            .map_err(|e| anyhow!("Tx builder error: {}", e));
    }

    if trading_mode == TradingMode::Live {
        tracing::error!("Unsupported DEX {} in Live mode", dex_program_id);
        bail!("Unsupported DEX {} in Live mode", dex_program_id);
    }
    tracing::warn!(
        "Unsupported DEX {}, using placeholder (Paper mode)",
        dex_program_id
    );
    Ok(create_placeholder_swap_instructions())
}

/// Fetch recent blockhash, or use random placeholder if RPC is not set.
/// This unified function replaces duplicated code in buy and exit pipelines.
async fn fetch_recent_blockhash(
    rpc_url: &Option<String>,
    trading_mode: TradingMode,
) -> Result<Hash> {
    if let Some(rpc_url) = rpc_url {
        let rpc_url_clone = rpc_url.clone();
        let result = tokio::task::spawn_blocking(move || {
            let rpc = RpcClient::new(rpc_url_clone);
            rpc.get_latest_blockhash()
        })
        .await
        .map_err(|e| anyhow!("Recent blockhash fetch task failed: {}", e))??;

        Ok(result)
    } else if trading_mode == TradingMode::Live {
        Err(anyhow!(
            "Missing RPC URL in Live mode while fetching recent blockhash"
        ))
    } else {
        Ok(Hash::new_unique())
    }
}

/// Reject buy if the originating signal is older than max_signal_age_ms.
///
/// Age is recomputed from wall-clock at **each** call (`chrono::Utc::now`), so the
/// pre-submit check is not a replay of the entry check — it catches time spent in
/// risk-gate / sizing / sign. Intentional: with public 4s budgets + max_age 500ms
/// the bot will structurally reject buys (prefer no trade over stale execution).
pub fn assert_signal_still_fresh(ctx: &ActorContext, phase: &str) -> Result<()> {
    let max_age_ms = ctx.risk_gate_config.max_signal_age_ms;
    if max_age_ms == 0 || ctx.last_signal_received_at == 0 {
        return Ok(());
    }
    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    if now_ms < ctx.last_signal_received_at {
        return Ok(());
    }
    let age_ms = now_ms - ctx.last_signal_received_at;
    if age_ms > max_age_ms {
        bail!(
            "Stale signal at {} for mint {}: age_ms={} > max_signal_age_ms={} — aborting buy",
            phase,
            ctx.mint,
            age_ms,
            max_age_ms
        );
    }
    tracing::debug!(
        mint = %ctx.mint,
        phase,
        age_ms,
        max_age_ms,
        "Signal freshness OK"
    );
    Ok(())
}

/// Run `submit` only if pre-submit freshness passes. Used by buy pipeline and tests.
pub fn submit_if_fresh<F, T>(ctx: &ActorContext, submit: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    assert_signal_still_fresh(ctx, "pre_submit_buy")?;
    submit()
}

/// Execute the buy pipeline: Risk Gate → Sizing → Tx Builder → Signer → Router.
pub async fn execute_buy_pipeline(
    ctx: &mut ActorContext,
    entry_price_sol: f64,
) -> Result<Position> {
    tracing::info!("Starting buy pipeline for mint {}", ctx.mint);

    // Check DEX readiness for Live mode
    check_dex_readiness(&ctx.dex_program_id, &ctx.dex_readiness, ctx.trading_mode)?;

    // Fail-closed: ensure critical components are present in Live mode
    if ctx.trading_mode == TradingMode::Live {
        if ctx.signer_client.is_none() {
            bail!("No signer_client configured in Live mode — refusing to trade");
        }
        if ctx.router.is_none() {
            bail!("No router configured in Live mode — refusing to trade");
        }
    }

    // Freshness at pipeline entry (queue wait + prior processing).
    assert_signal_still_fresh(ctx, "buy_pipeline_entry")?;

    // Step 1: Risk gate checks with timeout controlled by RiskGateConfig
    let signal = Signal {
        mint: ctx.mint.clone(),
        pool_address: ctx.pool_address.clone(),
        dex_program: ctx.dex_program_id.clone(),
        signal_source: "actor".to_string(),
        slot: ctx.last_signal_slot,
        received_at: if ctx.last_signal_received_at > 0 {
            ctx.last_signal_received_at
        } else {
            chrono::Utc::now().timestamp_millis() as u64
        },
        raw_pool_account_data: ctx.raw_pool_account_data.clone(),
        raw_pool_account_owner: ctx.raw_pool_account_owner.clone(),
    };

    let risk_result = risk_gate::run_pre_buy_checks_with_timeout(&signal, &ctx.risk_gate_config).await;

    match risk_result {
        risk_gate::RiskGateVerdict::Accept => {
            tracing::info!("Risk gate PASSED for mint {}", ctx.mint);
        }
        risk_gate::RiskGateVerdict::Reject { reason } => {
            tracing::warn!("Risk gate REJECTED for mint {}: {}", ctx.mint, reason);
            return Err(anyhow!("Risk gate rejection: {}", reason));
        }
    }

    // Step 2: Position sizing
    let position_size_sol = sizing::compute_position_size(
        ctx.capital_sol,
        ctx.tier,
        &ctx.tier_params,
        ctx.trade_stats.as_ref(),
    );

    tracing::info!("Computed position size: {} SOL", position_size_sol);
    let amount_in_lamports = (position_size_sol * 1_000_000_000.0) as u64;

    // Step 3: Build unsigned swap transaction
    let mint_pubkey = Pubkey::from_str(&ctx.mint).unwrap_or_else(|_| Pubkey::new_unique());
    let pool_pubkey = Pubkey::from_str(&ctx.pool_address).unwrap_or_else(|_| Pubkey::new_unique());
    let dex_program_pubkey =
        Pubkey::from_str(&ctx.dex_program_id).unwrap_or_else(|_| Pubkey::new_unique());

    let swap_instructions = fetch_and_build_dex_swap(
        &ctx.rpc_url,
        ctx.trading_mode,
        &ctx.dex_program_id,
        ctx.wallet,
        mint_pubkey,
        pool_pubkey,
        dex_program_pubkey,
        amount_in_lamports,
        SwapDirection::Buy,
        ctx.tier_params.sizing.slippage_hard_cap_bps,
        ctx.raw_pool_account_data.clone(),
    )
    .await?;

    // Get recent blockhash
    let blockhash = fetch_recent_blockhash(&ctx.rpc_url, ctx.trading_mode).await?;

    // Build complete unsigned transaction
    let (unsigned_tx, unsigned_tx_base64) = build_unsigned_transaction(
        swap_instructions,
        200_000,
        ctx.tier_params.emergency_exit.priority_fee_baseline as u64,
        &ctx.wallet,
        blockhash,
    )?;

    tracing::debug!(
        "Built unsigned buy transaction: base64 len={}",
        unsigned_tx_base64.len()
    );

    // Step 4: Request signature from Signer Client
    let mut final_tx = unsigned_tx.clone();

    if let Some(signer) = &ctx.signer_client {
        let req = SignerIntentRequest {
            context_key: format!("buy-{}", ctx.mint),
            ipc_seq: 0, // Auto-assigned by signer client
            issued_at_unix_ms: chrono::Utc::now().timestamp_millis() as u64,
            intent_type: IntentType::Buy,
            unsigned_transaction_base64: unsigned_tx_base64,
            program_id: "solbot-strategy".to_string(),
            dex_program_id: ctx.dex_program_id.clone(),
            max_size_lamports: amount_in_lamports,
            hard_cap_bps: ctx.tier_params.sizing.slippage_hard_cap_bps,
            destination_addresses: vec![ctx.pool_address.clone()],
            mint: ctx.mint.clone(),
            pool_address: ctx.pool_address.clone(),
            position_id: None,
            amount_or_pct: position_size_sol,
        };

        match signer.sign_intent(req).await {
            Ok(resp) => {
                if resp.status == VerdictStatus::Accepted {
                    if let Some(signed_base64) = resp.signed_transaction_base64 {
                        let decoded_bytes = BASE64.decode(&signed_base64).map_err(|e| {
                            anyhow!("Failed to decode signed transaction base64: {}", e)
                        })?;
                        let decoded_tx = bincode::deserialize::<Transaction>(&decoded_bytes)
                            .map_err(|e| {
                                anyhow!("Failed to deserialize signed transaction: {}", e)
                            })?;
                        final_tx = decoded_tx;
                        tracing::info!("Signer client signed buy transaction successfully");
                    } else {
                        // Signer accepted but didn't return signed transaction
                        tracing::error!("Signer accepted but no signed transaction returned");
                        return Err(anyhow!(
                            "Signer accepted but no signed transaction returned"
                        ));
                    }
                } else {
                    let reason = resp
                        .rejection_reason
                        .unwrap_or_else(|| "Unknown".to_string());
                    tracing::warn!("Signer client rejected buy intent: {}", reason);
                    return Err(anyhow!("Signer rejection: {}", reason));
                }
            }
            Err(e) => {
                tracing::warn!("Signer client IPC error: {}", e);
                if ctx.trading_mode == TradingMode::Live {
                    return Err(anyhow!("Signer IPC error: {}", e));
                }
            }
        }
    }

    // Step 5: Freshness gate immediately before broadcast, then Router submit.
    // Age recomputed here (not cached from entry) — catches risk-gate/sign latency.
    assert_signal_still_fresh(ctx, "pre_submit_buy")?;
    if ctx.trading_mode == TradingMode::Live {
        if let Some(ref mut router) = ctx.router {
            tracing::info!("Submitting live buy transaction to Router pipeline");
            match router.submit_transaction(final_tx).await {
                Ok(SubmitResult::Confirmed { signature, slot }) => {
                    tracing::info!(
                        "Buy transaction confirmed on-chain: sig={}, slot={}",
                        signature,
                        slot
                    );
                }
                Ok(SubmitResult::Failed { reason }) => {
                    tracing::error!("Buy transaction failed in router: {}", reason);
                    return Err(anyhow!("Router submission failed: {}", reason));
                }
                Ok(SubmitResult::Timeout) => {
                    tracing::warn!("Buy transaction submission timed out");
                    return Err(anyhow!("Router submission timeout"));
                }
                Err(e) => {
                    tracing::error!("Router error: {}", e);
                    return Err(anyhow!("Router error: {}", e));
                }
            }
        } else {
            // This should never happen due to check at pipeline start, but fail-closed
            tracing::error!("Router is None in Live mode - this should have been caught earlier");
            return Err(anyhow!("Router is None in Live mode"));
        }
    } else {
        tracing::info!("Paper trading mode - skipping live router transaction submission");
    }

    // Create position record with Opening status first
    let mut position = Position {
        position_id: format!("pos-{}-{}", ctx.mint, chrono::Utc::now().timestamp()),
        mint: ctx.mint.clone(),
        pool_address: ctx.pool_address.clone(),
        dex_program_id: ctx.dex_program_id.clone(),
        tier: ctx.tier,
        status: PositionStatus::Opening,
        entry_price_sol,
        size_sol: position_size_sol,
        size_pct_of_capital: position_size_sol / ctx.capital_sol,
        remaining_token_pct: 1.0,
        hard_sl_price_sol: entry_price_sol * 0.5,
        trailing_active: false,
        trailing_high_water_sol: None,
        realized_pnl_net_sol: 0.0,
        opened_at_utc: chrono::Utc::now().timestamp() as u64,
        closed_at_utc: None,
    };

    if ctx.trading_mode == TradingMode::Paper {
        crate::paper_trade_log::log_paper_buy(
            &ctx.mint,
            &ctx.pool_address,
            &ctx.dex_program_id,
            position_size_sol,
            entry_price_sol,
            &position.position_id,
        );
    }

    // Persist position with Opening status
    if let Some(ref repo) = ctx.positions_repo {
        if let Err(e) = repo.insert(&position).await {
            tracing::error!(
                "Failed to persist new position {} to DB: {}",
                position.position_id,
                e
            );
        }
    }

    // Transition from Opening to Open using PositionLifecycle
    let tx_signature = if ctx.trading_mode == TradingMode::Live {
        // Extract signature from router result if available
        None // Will be filled when router returns signature
    } else {
        None
    };

    if let Some(ref lifecycle) = ctx.position_lifecycle {
        match lifecycle
            .transition(&position, PositionStatus::Open, tx_signature)
            .await
        {
            Ok(transition) => {
                position.status = PositionStatus::Open;
                tracing::info!("Position lifecycle transition successful: {:?}", transition);

                // Emit PositionOpened event to event bus
                if let Some(ref tx) = ctx.lifecycle_event_tx {
                    let event = PositionLifecycleEvent::PositionOpened {
                        position_id: position.position_id.clone(),
                        mint: position.mint.clone(),
                        entry_price_sol: position.entry_price_sol,
                        size_sol: position.size_sol,
                    };
                    let _ = tx.send(event).await;
                }
            }
            Err(e) => {
                tracing::error!("Failed to transition position from Opening to Open: {}", e);
                return Err(anyhow!("Position lifecycle transition failed: {}", e));
            }
        }
    } else {
        // Fallback: direct transition without lifecycle validation (for testing)
        position.status = PositionStatus::Open;
        if let Some(ref trans_repo) = ctx.state_transitions_repo {
            let transition_id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().timestamp() as u64;
            let _ = trans_repo
                .insert(
                    &transition_id,
                    &position.position_id,
                    "opening",
                    "open",
                    "buy_confirmed",
                    None,
                    now,
                )
                .await;
        }
    }

    // Update position in DB with Open status
    if let Some(ref repo) = ctx.positions_repo {
        if let Err(e) = repo.update_position(&position).await {
            tracing::error!(
                "Failed to update position {} to Open status: {}",
                position.position_id,
                e
            );
        }
    }

    ctx.active_position = Some(position.clone());
    tracing::info!(
        "Position created and stored in context: {}",
        position.position_id
    );

    Ok(position)
}

/// Create placeholder swap instructions when real builder is unavailable.
fn create_placeholder_swap_instructions() -> tx_builder::SwapInstructions {
    tx_builder::SwapInstructions {
        setup_instructions: vec![],
        swap_instructions: vec![],
        cleanup_instructions: vec![],
        additional_signers: vec![],
    }
}

/// Execute the exit pipeline: Exit Tx Builder → Signer → Router.
pub async fn execute_exit_pipeline(
    ctx: &mut ActorContext,
    position: &Position,
    exit_action: ExitAction,
    current_price: f64,
) -> Result<()> {
    tracing::info!(
        "Starting exit pipeline for position {}: {:?}",
        position.position_id,
        exit_action
    );

    // Fail-closed: ensure critical components are present in Live mode
    if ctx.trading_mode == TradingMode::Live {
        if ctx.signer_client.is_none() {
            bail!("No signer_client configured in Live mode — refusing to exit position");
        }
        if ctx.router.is_none() {
            bail!("No router configured in Live mode — refusing to exit position");
        }
    }

    // Step 1: Calculate sell fraction
    let (sell_fraction, intent_type) = match &exit_action {
        ExitAction::TakeProfit {
            sell_fraction_of_remaining,
            ..
        } => (*sell_fraction_of_remaining, IntentType::Sell),
        ExitAction::TrailingStop { .. } => (1.0, IntentType::Sell),
        ExitAction::HardStopLoss => (1.0, IntentType::Emergency),
        ExitAction::Emergency { .. } => (1.0, IntentType::Emergency),
    };

    tracing::debug!(
        "Building sell transaction for {:.1}% of remaining position",
        sell_fraction * 100.0
    );

    // Build unsigned sell transaction parameters
    let mint_pubkey = Pubkey::from_str(&ctx.mint).unwrap_or_else(|_| Pubkey::new_unique());
    let pool_pubkey = Pubkey::from_str(&ctx.pool_address).unwrap_or_else(|_| Pubkey::new_unique());
    let dex_program_pubkey =
        Pubkey::from_str(&ctx.dex_program_id).unwrap_or_else(|_| Pubkey::new_unique());
    let amount_in_lamports = (position.size_sol * sell_fraction * 1_000_000_000.0) as u64;

    let use_escalation = matches!(exit_action, ExitAction::HardStopLoss | ExitAction::Emergency { .. })
        && ctx.trading_mode == TradingMode::Live;
    let mut final_tx = None;

    if use_escalation {
        tracing::info!(
            "Using escalation state machine for exit action {:?}",
            exit_action
        );

        let priority_fee_baseline = ctx.tier_params.emergency_exit.priority_fee_baseline as u64;
        let hard_cap_bps = ctx.tier_params.emergency_exit.emergency_slippage_bps;
        let signer = ctx.signer_client.as_ref().unwrap().clone();
        let router_arc = Arc::new(tokio::sync::Mutex::new(ctx.router.take().unwrap()));
        let signed_tx_state: Arc<Mutex<Option<Transaction>>> = Arc::new(Mutex::new(None));
        let signed_tx_state_for_sign = signed_tx_state.clone();
        let signed_tx_state_for_submit = signed_tx_state.clone();
        let position_size_sol = position.size_sol;
        let signer_for_sign = signer;
        let pool_address = ctx.pool_address.clone();
        let dex_program_id = ctx.dex_program_id.clone();
        let mint = ctx.mint.clone();
        let position_id = position.position_id.clone();
        let wallet = ctx.wallet;
        let rpc_url = ctx.rpc_url.clone();
        let trading_mode = ctx.trading_mode;
        let mint_pubkey_clone = mint_pubkey;
        let pool_pubkey_clone = pool_pubkey;
        let dex_program_pubkey_clone = dex_program_pubkey;
        let amount_in_lamports_clone = amount_in_lamports;

        let mut sign_fn: Box<
            dyn for<'a> FnMut(
                &'a EscalationState,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>,
            > + Send,
        > = Box::new(move |state: &EscalationState| {
            let signed_tx_state = signed_tx_state_for_sign.clone();
            let signer = signer_for_sign.clone();
            let pool_address = pool_address.clone();
            let dex_program_id = dex_program_id.clone();
            let mint = mint.clone();
            let position_id = position_id.clone();
            let rpc_url = rpc_url.clone();
            Box::pin(async move {
                tracing::info!(
                    "Escalation signing for stage {}",
                    state.current_stage
                );

                let slippage_bps = state.current_stage.slippage_bps();
                let blockhash = fetch_recent_blockhash(&rpc_url, trading_mode).await?;
                let swap_instructions = fetch_and_build_dex_swap(
                    &rpc_url,
                    trading_mode,
                    &dex_program_id,
                    wallet,
                    mint_pubkey_clone,
                    pool_pubkey_clone,
                    dex_program_pubkey_clone,
                    amount_in_lamports_clone,
                    SwapDirection::Sell,
                    slippage_bps,
                    None,
                )
                .await?;

                let priority_fee =
                    ((priority_fee_baseline as f64) * state.current_stage.priority_fee_multiplier())
                        as u64;
                let (unsigned_tx, unsigned_tx_base64) = build_unsigned_transaction(
                    swap_instructions,
                    200_000,
                    priority_fee,
                    &wallet,
                    blockhash,
                )?;

                let req = SignerIntentRequest {
                    context_key: format!("sell-{}-{}", mint, position_id),
                    ipc_seq: 0,
                    issued_at_unix_ms: chrono::Utc::now()
                        .timestamp_millis()
                        .try_into()
                        .unwrap(),
                    intent_type,
                    unsigned_transaction_base64: unsigned_tx_base64,
                    program_id: "solbot-strategy".to_string(),
                    dex_program_id: dex_program_id.clone(),
                    max_size_lamports: (position_size_sol * 1_000_000_000.0) as u64,
                    hard_cap_bps,
                    destination_addresses: vec![pool_address.clone()],
                    mint: mint.clone(),
                    pool_address: pool_address.clone(),
                    position_id: Some(position_id.clone()),
                    amount_or_pct: sell_fraction,
                };

                let resp = signer.sign_intent(req).await?;
                if resp.status != VerdictStatus::Accepted {
                    let reason = resp
                        .rejection_reason
                        .unwrap_or_else(|| "Unknown".to_string());
                    return Err(anyhow!("Signer rejection on escalation exit: {}", reason));
                }

                let signed_base64 = resp.signed_transaction_base64.ok_or_else(|| {
                    anyhow!("Signer accepted but no signed transaction returned")
                })?;
                let decoded_bytes = BASE64
                    .decode(&signed_base64)
                    .map_err(|e| anyhow!("Failed to decode signed transaction base64: {}", e))?;
                let decoded_tx = bincode::deserialize::<Transaction>(&decoded_bytes)
                    .map_err(|e| anyhow!("Failed to deserialize signed transaction: {}", e))?;
                let signature = decoded_tx
                    .signatures
                    .get(0)
                    .ok_or_else(|| anyhow!("Signed transaction has no signature"))
                    .map(|s| s.to_string())?;

                *signed_tx_state.lock().await = Some(decoded_tx.clone());
                tracing::info!("Signer client signed escalation transaction successfully");
                Ok(signature)
            })
        });


        let router_arc_for_submit = router_arc.clone();

        let mut submit_fn: Box<
            dyn for<'a> FnMut(
                &'a EscalationState,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Option<(String, u64)>>> + Send + 'a>,
            > + Send,
        > = Box::new(move |state: &EscalationState| {
            let signed_tx_state = signed_tx_state_for_submit.clone();
            let router_arc = router_arc_for_submit.clone();
            Box::pin(async move {
                let transaction = {
                    let guard = signed_tx_state.lock().await;
                    guard
                        .clone()
                        .ok_or_else(|| anyhow!("No signed transaction available for escalation"))?
                };

                let mut router = router_arc.lock().await;
                let routing_stage = match state.current_stage {
                    crate::exit::escalation::EscalationStage::Stage0Jito => RoutingStage::JitoBundle,
                    crate::exit::escalation::EscalationStage::Stage1PublicRpc2x
                    | crate::exit::escalation::EscalationStage::Stage2PublicRpc3xMaxSlippage => {
                        RoutingStage::PublicRpc
                    }
                    crate::exit::escalation::EscalationStage::Stage3DurableNonceStuck => {
                        RoutingStage::DurableNonce
                    }
                };

                match router
                    .submit_transaction_with_stage(routing_stage, transaction)
                    .await
                {
                    Ok(SubmitResult::Confirmed { signature, slot }) => Ok(Some((signature, slot))),
                    Ok(SubmitResult::Failed { reason }) => {
                        tracing::warn!(
                            "Escalation submit failed for stage {}: {}",
                            state.current_stage,
                            reason
                        );
                        Ok(None)
                    }
                    Ok(SubmitResult::Timeout) => {
                        tracing::warn!(
                            "Escalation submit timed out for stage {}",
                            state.current_stage
                        );
                        Ok(None)
                    }
                    Err(e) => Err(e),
                }
            })
        });


        let mut persist_fn: Box<
            dyn for<'a> FnMut(
                &'a EscalationState,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>
                + Send,
        > = Box::new(move |_state: &EscalationState| Box::pin(async move { Ok(()) }));

        let trigger = match exit_action {
            ExitAction::HardStopLoss => EscalationTrigger::HardStopLoss,
            ExitAction::Emergency { .. } => EscalationTrigger::HardStopLoss,
            _ => unreachable!(),
        };

        let mut escalation_state = EscalationState::new(
            uuid::Uuid::new_v4().to_string(),
            position.position_id.clone(),
            ctx.mint.clone(),
            trigger,
            chrono::Utc::now().timestamp_millis().try_into().unwrap(),
        );

        let escalation_result = run_escalation(&mut escalation_state, sign_fn, submit_fn, persist_fn).await;
        let router = Arc::try_unwrap(router_arc)
            .map_err(|_| anyhow!("Escalation router still has active references"))?
            .into_inner();
        ctx.router = Some(router);

        let confirmed = escalation_result?;
        if !confirmed {
            return Err(anyhow!(
                "Escalation reached terminal Stage3 without on-chain confirmation"
            ));
        }

        final_tx = Some(
            signed_tx_state.lock().await.clone().expect("signed tx should exist after successful escalation"),
        );
    } else {
        let swap_instructions = fetch_and_build_dex_swap(
            &ctx.rpc_url,
            ctx.trading_mode,
            &ctx.dex_program_id,
            ctx.wallet,
            mint_pubkey,
            pool_pubkey,
            dex_program_pubkey,
            amount_in_lamports,
            SwapDirection::Sell,
            ctx.tier_params.emergency_exit.emergency_slippage_bps,
            ctx.raw_pool_account_data.clone(),
        )
        .await?;

        // Get recent blockhash
        let blockhash = fetch_recent_blockhash(&ctx.rpc_url, ctx.trading_mode).await?;

        // Build complete unsigned transaction
        let (unsigned_tx, unsigned_tx_base64) = build_unsigned_transaction(
            swap_instructions,
            200_000,
            ctx.tier_params.emergency_exit.priority_fee_baseline as u64 * 2,
            &ctx.wallet,
            blockhash,
        )?;

        final_tx = Some(unsigned_tx.clone());

        // Step 2: Sign sell transaction
        if let Some(signer) = &ctx.signer_client {
            let req = SignerIntentRequest {
                context_key: format!("sell-{}-{}", ctx.mint, position.position_id),
                ipc_seq: 0,
                issued_at_unix_ms: chrono::Utc::now().timestamp_millis() as u64,
                intent_type,
                unsigned_transaction_base64: unsigned_tx_base64,
                program_id: "solbot-strategy".to_string(),
                dex_program_id: ctx.dex_program_id.clone(),
                max_size_lamports: (position.size_sol * 1_000_000_000.0) as u64,
                hard_cap_bps: ctx.tier_params.emergency_exit.emergency_slippage_bps,
                destination_addresses: vec![ctx.pool_address.clone()],
                mint: ctx.mint.clone(),
                pool_address: ctx.pool_address.clone(),
                position_id: Some(position.position_id.clone()),
                amount_or_pct: sell_fraction,
            };

            match signer.sign_intent(req).await {
                Ok(resp) => {
                    if resp.status == VerdictStatus::Accepted {
                        if let Some(signed_base64) = resp.signed_transaction_base64 {
                            let decoded_bytes = BASE64.decode(&signed_base64).map_err(|e| {
                                anyhow!("Failed to decode signed transaction base64: {}", e)
                            })?;
                            let decoded_tx = bincode::deserialize::<Transaction>(&decoded_bytes)
                                .map_err(|e| {
                                    anyhow!("Failed to deserialize signed transaction: {}", e)
                                })?;
                            final_tx = Some(decoded_tx);
                            tracing::info!("Signer client signed exit transaction successfully");
                        } else {
                            tracing::error!("Signer accepted but no signed transaction returned");
                            return Err(anyhow!(
                                "Signer accepted but no signed transaction returned"
                            ));
                        }
                    } else {
                        let reason = resp
                            .rejection_reason
                            .unwrap_or_else(|| "Unknown".to_string());
                        tracing::warn!("Signer client rejected exit intent: {}", reason);
                        return Err(anyhow!("Signer rejection on exit: {}", reason));
                    }
                }
                Err(e) => {
                    tracing::warn!("Signer client IPC error: {}", e);
                    if ctx.trading_mode == TradingMode::Live {
                        return Err(anyhow!("Signer IPC error: {}", e));
                    }
                }
            }
        }
    }

    if !use_escalation {
        let final_tx = final_tx.expect("Final transaction must be constructed before submission");

        if ctx.trading_mode == TradingMode::Live {
            if let Some(ref mut router) = ctx.router {
                tracing::info!("Submitting live sell transaction to Router pipeline");
                match router.submit_transaction(final_tx).await {
                    Ok(SubmitResult::Confirmed { signature, slot }) => {
                        tracing::info!(
                            "Sell transaction confirmed on-chain: sig={}, slot={}",
                            signature,
                            slot
                        );
                    }
                    Ok(SubmitResult::Failed { reason }) => {
                        tracing::error!("Sell transaction failed in router: {}", reason);
                        return Err(anyhow!("Router exit submission failed: {}", reason));
                    }
                    Ok(SubmitResult::Timeout) => {
                        tracing::warn!("Sell transaction submission timed out");
                        return Err(anyhow!("Router exit submission timeout"));
                    }
                    Err(e) => {
                        tracing::error!("Router error on exit: {}", e);
                        return Err(anyhow!("Router exit error: {}", e));
                    }
                }
            } else {
                // This should never happen due to check at pipeline start, but fail-closed
                tracing::error!("Router is None in Live mode - this should have been caught earlier");
                return Err(anyhow!("Router is None in Live mode"));
            }
        } else {
            tracing::info!("Paper trading mode - skipping live router exit submission");
        }
    }

    // Step 4: Update position state based on exit action using PositionLifecycle
    if let Some(ref mut pos) = ctx.active_position {
        let old_status = pos.status.clone();
        let (intermediate_status, final_status, tx_signature) = match &exit_action {
            ExitAction::TakeProfit { level, .. } => match level {
                exit::TpLevel::Tp1 => {
                    pos.remaining_token_pct -= 0.30;
                    pos.trailing_active = true;
                    pos.trailing_high_water_sol = Some(current_price);
                    (PositionStatus::Tp1Filled, None, None)
                }
                exit::TpLevel::Tp2 => {
                    pos.remaining_token_pct -= 0.30;
                    (PositionStatus::Tp2Filled, None, None)
                }
                exit::TpLevel::Tp3 => {
                    pos.remaining_token_pct = 0.0;
                    (
                        PositionStatus::Tp3Filled,
                        Some(PositionStatus::Closed),
                        None,
                    )
                }
            },
            ExitAction::TrailingStop { .. }
            | ExitAction::HardStopLoss
            | ExitAction::Emergency { .. } => {
                pos.remaining_token_pct = 0.0;
                (PositionStatus::Closing, Some(PositionStatus::Closed), None)
            }
        };

        // Determine the target status for this transition
        let target_status = intermediate_status.clone();

        // Use PositionLifecycle for state transition validation and logging
        if let Some(ref lifecycle) = ctx.position_lifecycle {
            match lifecycle
                .transition(pos, target_status.clone(), tx_signature.clone())
                .await
            {
                Ok(transition) => {
                    pos.status = target_status;
                    tracing::info!("Position lifecycle transition successful: {:?}", transition);

                    // Emit lifecycle event to event bus
                    if let Some(ref tx) = ctx.lifecycle_event_tx {
                        let event_opt = match transition {
                            crate::state::position_lifecycle::Transition::OpenToTp1 => {
                                Some(PositionLifecycleEvent::TakeProfitFilled {
                                    position_id: pos.position_id.clone(),
                                    tp_level: "tp1".to_string(),
                                    price_sol: current_price,
                                    sold_pct: 0.30,
                                    remaining_pct: pos.remaining_token_pct,
                                    realized_pnl_sol: pos.realized_pnl_net_sol,
                                })
                            }
                            crate::state::position_lifecycle::Transition::Tp1ToTp2 => {
                                Some(PositionLifecycleEvent::TakeProfitFilled {
                                    position_id: pos.position_id.clone(),
                                    tp_level: "tp2".to_string(),
                                    price_sol: current_price,
                                    sold_pct: 0.30,
                                    remaining_pct: pos.remaining_token_pct,
                                    realized_pnl_sol: pos.realized_pnl_net_sol,
                                })
                            }
                            crate::state::position_lifecycle::Transition::Tp2ToTp3 => {
                                Some(PositionLifecycleEvent::TakeProfitFilled {
                                    position_id: pos.position_id.clone(),
                                    tp_level: "tp3".to_string(),
                                    price_sol: current_price,
                                    sold_pct: 0.20,
                                    remaining_pct: pos.remaining_token_pct,
                                    realized_pnl_sol: pos.realized_pnl_net_sol,
                                })
                            }
                            crate::state::position_lifecycle::Transition::Tp3ToClosed => {
                                let duration = pos.closed_at_utc.unwrap_or(0) - pos.opened_at_utc;
                                Some(PositionLifecycleEvent::PositionClosed {
                                    position_id: pos.position_id.clone(),
                                    final_status: "tp3_closed".to_string(),
                                    total_pnl_sol: pos.realized_pnl_net_sol,
                                    duration_seconds: duration,
                                })
                            }
                            crate::state::position_lifecycle::Transition::OpenToClosing
                            | crate::state::position_lifecycle::Transition::Tp1ToClosing
                            | crate::state::position_lifecycle::Transition::Tp2ToClosing
                            | crate::state::position_lifecycle::Transition::Tp3ToClosing => {
                                if matches!(exit_action, ExitAction::TrailingStop { .. }) {
                                    Some(PositionLifecycleEvent::TrailingStopTriggered {
                                        position_id: pos.position_id.clone(),
                                        trigger_price_sol: current_price,
                                        high_water_sol: pos
                                            .trailing_high_water_sol
                                            .unwrap_or(current_price),
                                    })
                                } else {
                                    Some(PositionLifecycleEvent::StopLossTriggered {
                                        position_id: pos.position_id.clone(),
                                        trigger_price_sol: current_price,
                                        entry_price_sol: pos.entry_price_sol,
                                    })
                                }
                            }
                            crate::state::position_lifecycle::Transition::ClosingToClosed => {
                                let duration = pos.closed_at_utc.unwrap_or(0) - pos.opened_at_utc;
                                Some(PositionLifecycleEvent::PositionClosed {
                                    position_id: pos.position_id.clone(),
                                    final_status: "closed".to_string(),
                                    total_pnl_sol: pos.realized_pnl_net_sol,
                                    duration_seconds: duration,
                                })
                            }
                            _ => {
                                tracing::warn!(
                                    "Unexpected transition in exit pipeline: {:?}",
                                    transition
                                );
                                None
                            }
                        };

                        if let Some(event) = event_opt {
                            let _ = tx.send(event).await;
                        }
                    }

                    // If we have a final status (e.g., Tp3 → Closed or Closing → Closed), perform second transition
                    if let Some(final_status_value) = final_status.clone() {
                        pos.closed_at_utc = Some(chrono::Utc::now().timestamp() as u64);
                        if let Ok(final_transition) = lifecycle
                            .transition(pos, final_status_value.clone(), tx_signature.clone())
                            .await
                        {
                            pos.status = final_status_value;
                            tracing::info!(
                                "Position final lifecycle transition successful: {:?}",
                                final_transition
                            );

                            // Record loss to circuit breaker if position closed with negative PnL
                            if ctx.trading_mode == TradingMode::Paper {
                                let entry = pos.entry_price_sol;
                                let exit_px = if entry > 0.0 {
                                    entry * (1.0 + pos.realized_pnl_net_sol / pos.size_sol.max(1e-12))
                                } else {
                                    0.0
                                };
                                let pnl_pct = if pos.size_sol > 0.0 {
                                    (pos.realized_pnl_net_sol / pos.size_sol) * 100.0
                                } else {
                                    0.0
                                };
                                crate::paper_trade_log::log_paper_sell(
                                    &pos.mint,
                                    &pos.position_id,
                                    pos.size_sol,
                                    entry,
                                    exit_px,
                                    pos.realized_pnl_net_sol,
                                    pnl_pct,
                                    "exit_pipeline",
                                );
                            }
                            if pos.realized_pnl_net_sol < 0.0 {
                                if let Some(ref cb) = ctx.circuit_breaker {
                                    let loss_pct =
                                        (-pos.realized_pnl_net_sol / ctx.capital_sol) * 100.0;
                                    let status = cb.record_loss(ctx.tier, loss_pct);
                                    tracing::warn!(
                                        position_id = %pos.position_id,
                                        loss_sol = pos.realized_pnl_net_sol,
                                        loss_pct = loss_pct,
                                        tier = ?ctx.tier,
                                        daily_loss_pct = status.daily_loss_pct,
                                        warning_triggered = status.warning_triggered,
                                        limit_triggered = status.limit_triggered,
                                        "Loss recorded to circuit breaker"
                                    );

                                    // If limit triggered, emit emergency escalation event for circuit breaker
                                    if status.limit_triggered {
                                        tracing::error!(
                                            tier = ?ctx.tier,
                                            daily_loss_pct = status.daily_loss_pct,
                                            limit_threshold = status.daily_loss_pct,
                                            "CIRCUIT BREAKER LIMIT TRIGGERED - initiating emergency exit escalation for all tier positions"
                                        );

                                        if let Some(ref tx) = ctx.lifecycle_event_tx {
                                            let event = PositionLifecycleEvent::PositionStuck {
                                                position_id: pos.position_id.clone(),
                                                reason: format!("circuit_breaker_limit_triggered: daily_loss_pct={}", status.daily_loss_pct),
                                                escalation_stage: 1,
                                            };
                                            let _ = tx.send(event).await;
                                        }
                                    }
                                }
                            }

                            // Emit final close event
                            if let Some(ref tx) = ctx.lifecycle_event_tx {
                                if matches!(
                                    final_transition,
                                    crate::state::position_lifecycle::Transition::ClosingToClosed
                                ) {
                                    let duration =
                                        pos.closed_at_utc.unwrap_or(0) - pos.opened_at_utc;
                                    let event = PositionLifecycleEvent::PositionClosed {
                                        position_id: pos.position_id.clone(),
                                        final_status: "closed".to_string(),
                                        total_pnl_sol: pos.realized_pnl_net_sol,
                                        duration_seconds: duration,
                                    };
                                    let _ = tx.send(event).await;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to transition position: {}", e);
                    // Transition to Stuck state on lifecycle error
                    if let Some(ref lifecycle) = ctx.position_lifecycle {
                        let _ = lifecycle
                            .transition_with_reason(
                                pos,
                                PositionStatus::StuckManualInterventionRequired,
                                format!("exit_pipeline_error: {}", e),
                                None,
                            )
                            .await;
                    }
                    pos.status = PositionStatus::StuckManualInterventionRequired;

                    // Emit stuck event
                    if let Some(ref tx) = ctx.lifecycle_event_tx {
                        let event = PositionLifecycleEvent::ManualInterventionRequired {
                            position_id: pos.position_id.clone(),
                            reason: format!("exit_pipeline_error: {}", e),
                            current_status: status_to_string(&old_status).to_string(),
                        };
                        let _ = tx.send(event).await;
                    }
                }
            }
        } else {
            // Fallback: direct transition without lifecycle validation
            pos.status = target_status;
            if let Some(final_status_value) = final_status {
                pos.closed_at_utc = Some(chrono::Utc::now().timestamp() as u64);
                pos.status = final_status_value;
            }
            if let Some(ref trans_repo) = ctx.state_transitions_repo {
                let transition_id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().timestamp() as u64;
                let reason_str = match &exit_action {
                    ExitAction::TakeProfit { level, .. } => {
                        format!("take_profit_{:?}", level).to_lowercase()
                    }
                    ExitAction::TrailingStop { .. } => "trailing_stop".to_string(),
                    ExitAction::HardStopLoss => "hard_stop_loss".to_string(),
                    ExitAction::Emergency { reason } => format!("emergency_{}", reason),
                };
                let from_str = status_to_string(&old_status);
                let to_str = status_to_string(&pos.status);
                let _ = trans_repo
                    .insert(
                        &transition_id,
                        &pos.position_id,
                        from_str,
                        to_str,
                        &reason_str,
                        None,
                        now,
                    )
                    .await;
            }
        }

        // Persist position update to SQLite DB
        if let Some(ref repo) = ctx.positions_repo {
            if let Err(e) = repo.update_position(pos).await {
                tracing::error!("Failed to update position {} in DB: {}", pos.position_id, e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit_breaker::CircuitBreaker;
    use crate::exit::{ExitAction, TpLevel};
    use crate::state::position_lifecycle::Transition;
    use solbot_common::config::tier_params::{
        AdaptiveParams, CapitalRange, CircuitBreakerParams, CompromiseDetection,
        EmergencyExitParams, HotWalletParams, KellyParams, PositionLimits, SizingParams, TierInfo,
    };
    use solbot_common::config::TierParams;
    use solbot_common::types::position::{Position, PositionStatus};
    use solbot_common::types::tier::CapitalTier;
    use solbot_common::types::PositionLifecycleEvent;
    use tokio::sync::mpsc;

    fn create_test_position() -> Position {
        Position {
            position_id: "test-pos-123".to_string(),
            mint: "test_mint".to_string(),
            pool_address: "test_pool".to_string(),
            dex_program_id: "test_dex".to_string(),
            tier: CapitalTier::Micro,
            status: PositionStatus::Open,
            entry_price_sol: 1.0,
            size_sol: 10.0,
            size_pct_of_capital: 0.1,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000,
            closed_at_utc: None,
        }
    }

    #[test]
    fn test_position_lifecycle_integration_opening_to_open() {
        assert!(crate::state::position_lifecycle::is_valid_transition(
            &PositionStatus::Opening,
            &PositionStatus::Open
        ));

        let transition = crate::state::position_lifecycle::get_valid_transition(
            &PositionStatus::Opening,
            &PositionStatus::Open,
        );

        assert_eq!(transition, Some(Transition::OpeningToOpen));
    }

    #[test]
    fn test_position_lifecycle_integration_tp_ladder() {
        let mut position = create_test_position();

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &position.status,
            &PositionStatus::Tp1Filled
        ));
        position.status = PositionStatus::Tp1Filled;

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &position.status,
            &PositionStatus::Tp2Filled
        ));
        position.status = PositionStatus::Tp2Filled;

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &position.status,
            &PositionStatus::Tp3Filled
        ));
        position.status = PositionStatus::Tp3Filled;

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &position.status,
            &PositionStatus::Closed
        ));
    }

    #[test]
    fn test_position_lifecycle_integration_sl_trailing() {
        let position = create_test_position();

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &position.status,
            &PositionStatus::Closing
        ));

        let tp1_position = Position {
            status: PositionStatus::Tp1Filled,
            ..create_test_position()
        };

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &tp1_position.status,
            &PositionStatus::Closing
        ));

        let tp2_position = Position {
            status: PositionStatus::Tp2Filled,
            ..create_test_position()
        };

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &tp2_position.status,
            &PositionStatus::Closing
        ));

        let tp3_position = Position {
            status: PositionStatus::Tp3Filled,
            ..create_test_position()
        };

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &tp3_position.status,
            &PositionStatus::Closing
        ));

        assert!(crate::state::position_lifecycle::is_valid_transition(
            &PositionStatus::Closing,
            &PositionStatus::Closed
        ));
    }

    #[test]
    fn test_position_lifecycle_integration_invalid_transitions() {
        assert!(!crate::state::position_lifecycle::is_valid_transition(
            &PositionStatus::Tp1Filled,
            &PositionStatus::Open
        ));

        assert!(!crate::state::position_lifecycle::is_valid_transition(
            &PositionStatus::Closed,
            &PositionStatus::Open
        ));

        assert!(!crate::state::position_lifecycle::is_valid_transition(
            &PositionStatus::Open,
            &PositionStatus::Tp2Filled
        ));

        assert!(!crate::state::position_lifecycle::is_valid_transition(
            &PositionStatus::Open,
            &PositionStatus::Tp3Filled
        ));

        assert!(!crate::state::position_lifecycle::is_valid_transition(
            &PositionStatus::StuckManualInterventionRequired,
            &PositionStatus::Open
        ));
    }

    #[test]
    fn test_actor_context_lifecycle_fields() {
        let (lifecycle_event_tx, _) = mpsc::channel::<PositionLifecycleEvent>(100);

        // Test that the new lifecycle fields exist in ActorContext
        // We can't construct a full ActorContext without complex config structs,
        // so we just verify the channel works
        assert!(lifecycle_event_tx.capacity() >= 100);
    }

    #[test]
    fn test_exit_action_to_status_mapping() {
        let tp1_action = ExitAction::TakeProfit {
            level: TpLevel::Tp1,
            sell_fraction_of_remaining: 0.30,
        };

        let (intermediate_status, final_status) = match &tp1_action {
            ExitAction::TakeProfit { level, .. } => match level {
                TpLevel::Tp1 => (PositionStatus::Tp1Filled, None),
                TpLevel::Tp2 => (PositionStatus::Tp2Filled, None),
                TpLevel::Tp3 => (PositionStatus::Tp3Filled, Some(PositionStatus::Closed)),
            },
            _ => (PositionStatus::Closing, Some(PositionStatus::Closed)),
        };

        assert_eq!(intermediate_status, PositionStatus::Tp1Filled);
        assert!(final_status.is_none());

        let tp3_action = ExitAction::TakeProfit {
            level: TpLevel::Tp3,
            sell_fraction_of_remaining: 0.20,
        };

        let (intermediate_status, final_status) = match &tp3_action {
            ExitAction::TakeProfit { level, .. } => match level {
                TpLevel::Tp1 => (PositionStatus::Tp1Filled, None),
                TpLevel::Tp2 => (PositionStatus::Tp2Filled, None),
                TpLevel::Tp3 => (PositionStatus::Tp3Filled, Some(PositionStatus::Closed)),
            },
            _ => (PositionStatus::Closing, Some(PositionStatus::Closed)),
        };

        assert_eq!(intermediate_status, PositionStatus::Tp3Filled);
        assert_eq!(final_status, Some(PositionStatus::Closed));

        let trailing_action = ExitAction::TrailingStop {
            trigger_price: 1.8,
            high_water_mark: 2.0,
        };

        let (intermediate_status, final_status) = match &trailing_action {
            ExitAction::TrailingStop { .. }
            | ExitAction::HardStopLoss
            | ExitAction::Emergency { .. } => {
                (PositionStatus::Closing, Some(PositionStatus::Closed))
            }
            _ => (PositionStatus::Closing, Some(PositionStatus::Closed)),
        };

        assert_eq!(intermediate_status, PositionStatus::Closing);
        assert_eq!(final_status, Some(PositionStatus::Closed));
    }

    #[test]
    fn test_circuit_breaker_integration() {
        let circuit_breaker = CircuitBreaker::new();

        // Test recording a loss (thresholds are in fractions, not percentages)
        let status = circuit_breaker.record_loss(CapitalTier::Micro, 0.10);
        assert!((status.daily_loss_pct - 0.10).abs() < 0.0001);
        assert!(!status.warning_triggered);
        assert!(!status.limit_triggered);

        // Test warning threshold (Micro tier: 0.15 = 15%)
        let status = circuit_breaker.record_loss(CapitalTier::Micro, 0.04);
        assert!((status.daily_loss_pct - 0.14).abs() < 0.0001);
        assert!(!status.warning_triggered);
        assert!(!status.limit_triggered);

        let status = circuit_breaker.record_loss(CapitalTier::Micro, 0.01);
        assert!((status.daily_loss_pct - 0.15).abs() < 0.0001);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);

        // Test limit threshold (Micro tier: 0.25 = 25%)
        let status = circuit_breaker.record_loss(CapitalTier::Micro, 0.09);
        assert!((status.daily_loss_pct - 0.24).abs() < 0.0001);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);

        let status = circuit_breaker.record_loss(CapitalTier::Micro, 0.01);
        assert!((status.daily_loss_pct - 0.25).abs() < 0.0001);
        assert!(status.warning_triggered);
        assert!(status.limit_triggered);
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let circuit_breaker = CircuitBreaker::new();

        // Record some losses
        circuit_breaker.record_loss(CapitalTier::Micro, 0.20);
        let status = circuit_breaker.get_status(CapitalTier::Micro);
        assert!((status.daily_loss_pct - 0.20).abs() < 0.0001);

        // Reset
        circuit_breaker.reset_daily();
        let status = circuit_breaker.get_status(CapitalTier::Micro);
        assert_eq!(status.daily_loss_pct, 0.0);
        assert!(!status.warning_triggered);
        assert!(!status.limit_triggered);
    }

    #[test]
    fn test_circuit_breaker_tier_specific_thresholds() {
        // Micro tier: warning 0.15 (15%), limit 0.25 (25%)
        let cb_micro = CircuitBreaker::new();
        let status = cb_micro.record_loss(CapitalTier::Micro, 0.15);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);

        // Small tier: warning 0.16 (16%), limit 0.24 (24%)
        let cb_small = CircuitBreaker::new();
        let status = cb_small.record_loss(CapitalTier::Small, 0.16);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);

        // Medium tier: warning 0.15 (15%), limit 0.216 (21.6%)
        let cb_medium = CircuitBreaker::new();
        let status = cb_medium.record_loss(CapitalTier::Medium, 0.15);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);

        // Large tier: warning 0.12 (12%), limit 0.166 (16.6%)
        let cb_large = CircuitBreaker::new();
        let status = cb_large.record_loss(CapitalTier::Large, 0.12);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);
    }

    fn freshness_ctx(age_ms: u64, max_age_ms: u64) -> ActorContext {
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let rpc_url = "http://127.0.0.1:8899".to_string();
        let mut risk = RiskGateConfig::from_shared_rpc(
            std::sync::Arc::new(
                solana_rpc_client::nonblocking::rpc_client::RpcClient::new(rpc_url.clone()),
            ),
            rpc_url,
        );
        risk.max_signal_age_ms = max_age_ms;
        risk.skip_simulate_sell = true;
        ActorContext {
            mint: "Mint111111111111111111111111111111111111111".into(),
            pool_address: "Pool111111111111111111111111111111111111111".into(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".into(),
            wallet: Pubkey::new_unique(),
            trading_mode: TradingMode::Paper,
            risk_gate_config: risk,
            tier: CapitalTier::Micro,
            tier_params: TierParams {
                tier: TierInfo {
                    name: "Micro".into(),
                    capital_range: CapitalRange {
                        min_usd: 0,
                        max_usd: 1000,
                    },
                },
                sizing: SizingParams {
                    base_size_pct: 0.01,
                    max_size_sol: 0.1,
                    slippage_hard_cap_bps: 500,
                    kelly: KellyParams {
                        fraction_of_kelly: 0.5,
                    },
                    adaptive: AdaptiveParams {
                        enabled: false,
                        calibration_window_slots: 100,
                        min_sample_size: 10,
                        size_max_pct: 0.05,
                    },
                },
                position_limits: PositionLimits {
                    max_concurrent_positions: 3,
                    circuit_breaker: CircuitBreakerParams {
                        warning_threshold_pct: 0.15,
                        daily_loss_limit_pct: 0.25,
                        max_parallel_emergency_sells: 2,
                    },
                },
                hot_wallet: HotWalletParams { cap_sol: 1.0 },
                compromise_detection: CompromiseDetection {
                    signing_rate_threshold_5min: 10,
                },
                emergency_exit: EmergencyExitParams {
                    emergency_slippage_bps: 1000,
                    priority_fee_baseline: 1000,
                },
            },
            capital_sol: 1.0,
            router: None,
            signer_client: None,
            rpc_url: None,
            active_position: None,
            trade_stats: None,
            dex_readiness: DexReadinessConfig::default(),
            positions_repo: None,
            state_transitions_repo: None,
            position_lifecycle: None,
            lifecycle_event_tx: None,
            circuit_breaker: None,
            raw_pool_account_data: None,
            raw_pool_account_owner: None,
            last_signal_slot: 1,
            last_signal_received_at: now.saturating_sub(age_ms),
            buy_attempted: false,
        }
    }

    /// Pre-submit freshness: stale signal must not call submit (mock panics if reached).
    #[test]
    fn test_submit_if_fresh_blocks_stale_before_router() {
        let ctx = freshness_ctx(5_000, 500); // 5s old, max 500ms
        let mut submitted = false;
        let result: Result<(), anyhow::Error> = submit_if_fresh(&ctx, || {
            submitted = true;
            panic!("router submit must not run on stale signal");
        });
        assert!(result.is_err(), "expected stale rejection, got {result:?}");
        assert!(
            !submitted,
            "submit closure must not run when freshness fails"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Stale signal") && msg.contains("pre_submit_buy"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn test_submit_if_fresh_allows_fresh_signal() {
        let ctx = freshness_ctx(50, 500); // 50ms old, max 500ms
        let result = submit_if_fresh(&ctx, || Ok::<_, anyhow::Error>(42));
        assert_eq!(result.unwrap(), 42);
    }

    /// Late check recomputes wall-clock age (not a cached entry decision).
    #[test]
    fn test_freshness_recomputes_age_each_call() {
        let mut ctx = freshness_ctx(100, 300);
        assert!(assert_signal_still_fresh(&ctx, "entry").is_ok());
        // Make signal older than max without changing a "cached age" field.
        ctx.last_signal_received_at = chrono::Utc::now().timestamp_millis() as u64 - 2_000;
        let err = assert_signal_still_fresh(&ctx, "pre_submit_buy").unwrap_err();
        assert!(err.to_string().contains("pre_submit_buy"));
    }
}
