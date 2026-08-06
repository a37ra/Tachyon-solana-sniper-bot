//! MintActor trading loop and pipeline execution (Stage 5.1).
//!
//! Executes:
//! - Buy pipeline: Risk Gate (150ms timeout) → Position Sizing → Tx Builder → Signer Client → Transaction Router
//! - Sell pipeline: Exit Evaluation → Exit Tx Builder → Signer Client → Transaction Router → State Transition

use super::{market_data_lane::MarketDataLane, ActorConfig};
use crate::circuit_breaker::CircuitBreaker;
use crate::exit::{self, escalation::EscalationTrigger, ExitAction};
use crate::risk_gate::{self, RiskGateConfig};
use crate::routing::{SubmitResult, TransactionRouter};
use crate::signer_client::SignerClient;
use crate::sizing::{self, TradeStats};
use crate::state::position_lifecycle::PositionLifecycle;
use crate::tx_builder::{self, SwapDirection, SwapParams, build_unsigned_transaction, apply_slippage};
use crate::tx_builder::pump_fun as pump_fun_builder;
use crate::tx_builder::raydium_amm_v4 as raydium_amm_v4_builder;
use crate::tx_builder::raydium_cpmm as raydium_cpmm_builder;
use crate::tx_builder::meteora_dlmm as meteora_dlmm_builder;
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::hash::Hash;
use solana_transaction::Transaction;
use solbot_common::config::TierParams;
use solbot_common::config::base_config::DexReadinessConfig;
use solbot_common::types::intent::{IntentType, SignerIntentRequest, VerdictStatus};
use solbot_common::types::position::{Position, PositionStatus};
use solbot_common::types::tier::CapitalTier;
use solbot_common::types::{ControlEvent, MarketDataEvent, PositionLifecycleEvent, Signal};
use crate::state::positions_repo::{status_to_string, PositionsRepo};
use crate::state::state_transitions_repo::StateTransitionsRepo;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

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
        let balance_a = rpc.get_token_account_balance(&vault_a)
            .map_err(|e| anyhow!("Failed to fetch {} vault_a balance: {}", dex_name, e))
            .and_then(|r| r.amount.parse::<u64>().map_err(|e| anyhow!("Failed to parse {} vault_a balance: {}", dex_name, e)))?;
        let balance_b = rpc.get_token_account_balance(&vault_b)
            .map_err(|e| anyhow!("Failed to fetch {} vault_b balance: {}", dex_name, e))
            .and_then(|r| r.amount.parse::<u64>().map_err(|e| anyhow!("Failed to parse {} vault_b balance: {}", dex_name, e)))?;
        Ok::<(u64, u64), anyhow::Error>((balance_a, balance_b))
    }).await.map_err(|e| anyhow!("{} vault balance fetch task failed: {}", dex_name_clone, e))??;
    Ok(vault_balances)
}

/// Check if the DEX is ready for Live trading based on program ID.
fn check_dex_readiness(dex_program_id: &str, dex_readiness: &DexReadinessConfig, trading_mode: TradingMode) -> Result<()> {
    if trading_mode == TradingMode::Paper {
        return Ok(()); // Always allow in Paper mode
    }
    
    let readiness = if dex_program_id == raydium_amm_v4_builder::RAYDIUM_AMM_V4_PROGRAM.to_string() {
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
    pub signer_client: Option<SignerClient>,
    pub rpc_url: Option<String>,
    pub active_position: Option<Position>,
    pub trade_stats: Option<TradeStats>,
    pub dex_readiness: DexReadinessConfig,
    pub positions_repo: Option<Arc<PositionsRepo>>,
    pub state_transitions_repo: Option<Arc<StateTransitionsRepo>>,
    pub position_lifecycle: Option<Arc<PositionLifecycle>>,
    pub lifecycle_event_tx: Option<tokio::sync::mpsc::Sender<PositionLifecycleEvent>>,
    pub circuit_breaker: Option<Arc<CircuitBreaker>>,
}

pub async fn run(
    mint: String,
    mut control_rx: tokio::sync::mpsc::Receiver<ControlEvent>,
    mut market_data_rx: MarketDataLane,
    mut ctx: ActorContext,
    config: ActorConfig,
) {
    tracing::info!("Starting mint actor for {} (mode: {:?})", mint, ctx.trading_mode);

    // Spawn daily circuit breaker reset task (00:00 UTC)
    if let Some(ref circuit_breaker) = ctx.circuit_breaker {
        let cb_clone = circuit_breaker.clone();
        tokio::spawn(async move {
            loop {
                // Calculate time until next 00:00 UTC
                let now = chrono::Utc::now();
                let tomorrow = now.date_naive() + chrono::Duration::days(1);
                let next_midnight = tomorrow.and_hms_opt(0, 0, 0).unwrap().and_utc();
                let duration_until_midnight = (next_midnight - now).to_std().unwrap_or(std::time::Duration::from_secs(86400));
                
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
        ControlEvent::MigrationEvent { mint: _, new_pool_address } => {
            tracing::info!("Migration event for {}: new pool = {}", mint, new_pool_address);
            ctx.pool_address = new_pool_address;
        }
        ControlEvent::KillSwitch { activated, reason } => {
            tracing::warn!("Kill switch for {}: activated={}, reason={}", mint, activated, reason);
            if activated {
                if let Some(ref pos) = ctx.active_position.clone() {
                    tracing::warn!("Emergency exit for position {} due to kill switch", pos.position_id);
                    let exit_action = ExitAction::Emergency { reason };
                    let current_price = pos.entry_price_sol;
                    let _ = execute_exit_pipeline(ctx, &pos, exit_action, current_price).await;
                }
            }
        }
        ControlEvent::TpTriggerRecompute { position_id } => {
            tracing::info!("TP trigger recompute for {}: position_id={}", mint, position_id);
        }
        ControlEvent::SlTriggerRecompute { position_id } => {
            tracing::info!("SL trigger recompute for {}: position_id={}", mint, position_id);
        }
        ControlEvent::ManualCliCommand { command, args } => {
            tracing::info!("Manual CLI command for {}: cmd={:?}, args={:?}", mint, command, args);
        }
    }
}

pub async fn process_market_data_event(mint: &str, event: MarketDataEvent, ctx: &mut ActorContext) {
    match event {
        MarketDataEvent::PriceTick { mint: _, pool_address, price_sol, timestamp } => {
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
                tracing::info!("No active position for {}, triggering buy pipeline at price {} SOL", mint, price_sol);
                if let Err(e) = execute_buy_pipeline(ctx, price_sol).await {
                    tracing::warn!("Buy pipeline failed for {}: {}", mint, e);
                }
            } else if let Some(ref position) = ctx.active_position.clone() {
                if position.status != PositionStatus::Closed && position.status != PositionStatus::StuckManualInterventionRequired {
                    if let Some(exit_action) = exit::evaluate_exit(position, price_sol) {
                        tracing::info!(
                            "Exit action triggered for position {}: {:?}",
                            position.position_id,
                            exit_action
                        );
                        if let Err(e) = execute_exit_pipeline(ctx, position, exit_action, price_sol).await {
                            tracing::error!("Exit pipeline failed for position {}: {}", position.position_id, e);
                            if let Some(ref mut pos) = ctx.active_position {
                                let old_status = pos.status.clone();
                                
                                // Use PositionLifecycle for transition to Stuck state
                                if let Some(ref lifecycle) = ctx.position_lifecycle {
                                    let _ = lifecycle.transition_with_reason(
                                        pos,
                                        PositionStatus::StuckManualInterventionRequired,
                                        format!("exit_pipeline_error: {}", e),
                                        None,
                                    ).await;
                                }
                                
                                pos.status = PositionStatus::StuckManualInterventionRequired;
                                
                                if let Some(ref repo) = ctx.positions_repo {
                                    let _ = repo.update_position(pos).await;
                                }
                                
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
                    }
                }
            }
        }
        MarketDataEvent::LiquidityUpdate { mint: _, pool_address, liquidity_usd } => {
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
            dex_name, direction
        );
    }
    Ok(())
}

/// Execute the buy pipeline: Risk Gate → Sizing → Tx Builder → Signer → Router.
pub async fn execute_buy_pipeline(ctx: &mut ActorContext, entry_price_sol: f64) -> Result<Position> {
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

    // Step 1: Risk gate checks with 150ms hard timeout
    let signal = Signal {
        mint: ctx.mint.clone(),
        pool_address: ctx.pool_address.clone(),
        dex_program: ctx.dex_program_id.clone(),
        signal_source: "actor".to_string(),
        slot: 0,
        received_at: chrono::Utc::now().timestamp() as u64,
    };

    let risk_result = tokio::time::timeout(
        Duration::from_millis(150),
        risk_gate::run_pre_buy_checks_with_timeout(&signal, &ctx.risk_gate_config),
    )
    .await;

    match risk_result {
        Ok(risk_gate::RiskGateVerdict::Accept) => {
            tracing::info!("Risk gate PASSED for mint {}", ctx.mint);
        }
        Ok(risk_gate::RiskGateVerdict::Reject { reason }) => {
            tracing::warn!("Risk gate REJECTED for mint {}: {}", ctx.mint, reason);
            return Err(anyhow!("Risk gate rejection: {}", reason));
        }
        Err(_) => {
            tracing::warn!("Risk gate TIMEOUT (150ms) for mint {}", ctx.mint);
            return Err(anyhow!("Risk gate timeout (150ms)"));
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
    let dex_program_pubkey = Pubkey::from_str(&ctx.dex_program_id).unwrap_or_else(|_| Pubkey::new_unique());

    // Fetch pool state from RPC for Pump.fun
    let swap_instructions = if let Some(ref rpc_url) = ctx.rpc_url {
        if ctx.dex_program_id == pump_fun_builder::PUMP_FUN_PROGRAM.to_string() {
            // Fetch Pump.fun bonding curve state (blocking call wrapped in spawn_blocking)
            let rpc_url_clone = rpc_url.clone();
            let pool_pubkey_clone = pool_pubkey;
            let pool_account = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_clone);
                rpc.get_account(&pool_pubkey_clone)
            }).await.ok().and_then(|r| r.ok());
            if let Some(account) = pool_account {
                if let Ok(curve) = pump_fun_builder::PumpFunBondingCurve::from_account_data(&account.data, &pool_pubkey) {
                    let expected_out = curve.calculate_buy_output(amount_in_lamports);
                    let minimum_amount_out = apply_slippage(expected_out, ctx.tier_params.sizing.slippage_hard_cap_bps);
                    
                    let swap_params = SwapParams {
                        mint: mint_pubkey,
                        pool_address: pool_pubkey,
                        dex_program_id: dex_program_pubkey,
                        wallet: ctx.wallet,
                        direction: SwapDirection::Buy,
                        amount_in: amount_in_lamports,
                        minimum_amount_out,
                        slippage_bps: ctx.tier_params.sizing.slippage_hard_cap_bps,
                    };
                    match pump_fun_builder::build_swap(&swap_params, &curve) {
                        Ok(ixs) => ixs,
                        Err(e) => {
                            tracing::error!("Failed to build Pump.fun swap: {}", e);
                            return Err(anyhow!("Tx builder error: {}", e));
                        }
                    }
                } else {
                    if ctx.trading_mode == TradingMode::Live {
                        tracing::error!("Failed to parse Pump.fun bonding curve in Live mode");
                        return Err(anyhow!("Failed to parse Pump.fun bonding curve in Live mode"));
                    }
                    tracing::warn!("Failed to parse Pump.fun bonding curve, using placeholder (Paper mode)");
                    create_placeholder_swap_instructions()
                }
            } else {
                if ctx.trading_mode == TradingMode::Live {
                    tracing::error!("Failed to fetch pool account in Live mode");
                    return Err(anyhow!("Failed to fetch pool account in Live mode"));
                }
                tracing::warn!("Failed to fetch pool account, using placeholder (Paper mode)");
                create_placeholder_swap_instructions()
            }
        } else if ctx.dex_program_id == raydium_amm_v4_builder::RAYDIUM_AMM_V4_PROGRAM.to_string() {
            // Fetch Raydium AMM v4 pool state
            let rpc_url_clone = rpc_url.clone();
            let pool_pubkey_clone = pool_pubkey;
            let pool_account = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_clone);
                rpc.get_account(&pool_pubkey_clone)
            }).await.ok().and_then(|r| r.ok());
            if let Some(account) = pool_account {
                if let Ok(mut pool_state) = raydium_amm_v4_builder::RaydiumAmmV4PoolState::from_account_data(&account.data) {
                    // Fetch vault balances for accurate pricing (fail-closed)
                    let vault_balances = fetch_vault_balances(
                        rpc_url.clone(),
                        pool_state.serum_coin_vault,
                        pool_state.serum_pc_vault,
                        "Raydium AMM v4",
                    ).await?;
                    
                    pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);
                    
                    let expected_out = pool_state.calculate_buy_output(amount_in_lamports);
                    ensure_nonzero_output(expected_out, "Raydium AMM v4", "buy")?;
                    let minimum_amount_out = apply_slippage(expected_out, ctx.tier_params.sizing.slippage_hard_cap_bps);
                    
                    let swap_params = SwapParams {
                        mint: mint_pubkey,
                        pool_address: pool_pubkey,
                        dex_program_id: dex_program_pubkey,
                        wallet: ctx.wallet,
                        direction: SwapDirection::Buy,
                        amount_in: amount_in_lamports,
                        minimum_amount_out,
                        slippage_bps: ctx.tier_params.sizing.slippage_hard_cap_bps,
                    };
                    match raydium_amm_v4_builder::build_swap(&swap_params, &pool_state) {
                        Ok(ixs) => ixs,
                        Err(e) => {
                            tracing::error!("Failed to build Raydium AMM v4 swap: {}", e);
                            return Err(anyhow!("Tx builder error: {}", e));
                        }
                    }
                } else {
                    if ctx.trading_mode == TradingMode::Live {
                        tracing::error!("Failed to parse Raydium AMM v4 pool state in Live mode");
                        return Err(anyhow!("Failed to parse Raydium AMM v4 pool state in Live mode"));
                    }
                    tracing::warn!("Failed to parse Raydium AMM v4 pool state, using placeholder (Paper mode)");
                    create_placeholder_swap_instructions()
                }
            } else {
                if ctx.trading_mode == TradingMode::Live {
                    tracing::error!("Failed to fetch pool account in Live mode");
                    return Err(anyhow!("Failed to fetch pool account in Live mode"));
                }
                tracing::warn!("Failed to fetch pool account, using placeholder (Paper mode)");
                create_placeholder_swap_instructions()
            }
        } else if ctx.dex_program_id == raydium_cpmm_builder::RAYDIUM_CPMM_PROGRAM.to_string() {
            // Fetch Raydium CPMM pool state
            let rpc_url_clone = rpc_url.clone();
            let pool_pubkey_clone = pool_pubkey;
            let pool_account = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_clone);
                rpc.get_account(&pool_pubkey_clone)
            }).await.ok().and_then(|r| r.ok());
            if let Some(account) = pool_account {
                if let Ok(mut pool_state) = raydium_cpmm_builder::CpmmPoolState::from_account_data(&account.data) {
                    // Fetch vault balances for accurate pricing (fail-closed)
                    let vault_balances = fetch_vault_balances(
                        rpc_url.clone(),
                        pool_state.token_0_vault,
                        pool_state.token_1_vault,
                        "Raydium CPMM",
                    ).await?;
                    
                    pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);
                    
                    let expected_out = pool_state.calculate_buy_output(amount_in_lamports);
                    ensure_nonzero_output(expected_out, "Raydium CPMM", "buy")?;
                    let minimum_amount_out = apply_slippage(expected_out, ctx.tier_params.sizing.slippage_hard_cap_bps);
                    
                    let swap_params = SwapParams {
                        mint: mint_pubkey,
                        pool_address: pool_pubkey,
                        dex_program_id: dex_program_pubkey,
                        wallet: ctx.wallet,
                        direction: SwapDirection::Buy,
                        amount_in: amount_in_lamports,
                        minimum_amount_out,
                        slippage_bps: ctx.tier_params.sizing.slippage_hard_cap_bps,
                    };
                    match raydium_cpmm_builder::build_swap(&swap_params, &pool_state) {
                        Ok(ixs) => ixs,
                        Err(e) => {
                            tracing::error!("Failed to build Raydium CPMM swap: {}", e);
                            return Err(anyhow!("Tx builder error: {}", e));
                        }
                    }
                } else {
                    if ctx.trading_mode == TradingMode::Live {
                        tracing::error!("Failed to parse Raydium CPMM pool state in Live mode");
                        return Err(anyhow!("Failed to parse Raydium CPMM pool state in Live mode"));
                    }
                    tracing::warn!("Failed to parse Raydium CPMM pool state, using placeholder (Paper mode)");
                    create_placeholder_swap_instructions()
                }
            } else {
                if ctx.trading_mode == TradingMode::Live {
                    tracing::error!("Failed to fetch pool account in Live mode");
                    return Err(anyhow!("Failed to fetch pool account in Live mode"));
                }
                tracing::warn!("Failed to fetch pool account, using placeholder (Paper mode)");
                create_placeholder_swap_instructions()
            }
        } else if ctx.dex_program_id == meteora_dlmm_builder::METEORA_DLMM_PROGRAM.to_string() {
            // Fetch Meteora DLMM pool state
            let rpc_url_clone = rpc_url.clone();
            let pool_pubkey_clone = pool_pubkey;
            let pool_account = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_clone);
                rpc.get_account(&pool_pubkey_clone)
            }).await.ok().and_then(|r| r.ok());
            if let Some(account) = pool_account {
                if let Ok(mut pool_state) = meteora_dlmm_builder::DlmmPoolState::from_account_data(&account.data, &pool_pubkey) {
                    // Fetch vault balances for accurate pricing (fail-closed)
                    let vault_balances = fetch_vault_balances(
                        rpc_url.clone(),
                        pool_state.reserve_x,
                        pool_state.reserve_y,
                        "Meteora DLMM",
                    ).await?;
                    
                    pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);
                    
                    let expected_out = pool_state.calculate_buy_output(amount_in_lamports);
                    ensure_nonzero_output(expected_out, "Meteora DLMM", "buy")?;
                    let minimum_amount_out = apply_slippage(expected_out, ctx.tier_params.sizing.slippage_hard_cap_bps);
                    
                    let swap_params = SwapParams {
                        mint: mint_pubkey,
                        pool_address: pool_pubkey,
                        dex_program_id: dex_program_pubkey,
                        wallet: ctx.wallet,
                        direction: SwapDirection::Buy,
                        amount_in: amount_in_lamports,
                        minimum_amount_out,
                        slippage_bps: ctx.tier_params.sizing.slippage_hard_cap_bps,
                    };
                    match meteora_dlmm_builder::build_swap(&swap_params, &pool_state) {
                        Ok(ixs) => ixs,
                        Err(e) => {
                            tracing::error!("Failed to build Meteora DLMM swap: {}", e);
                            return Err(anyhow!("Tx builder error: {}", e));
                        }
                    }
                } else {
                    if ctx.trading_mode == TradingMode::Live {
                        tracing::error!("Failed to parse Meteora DLMM pool state in Live mode");
                        return Err(anyhow!("Failed to parse Meteora DLMM pool state in Live mode"));
                    }
                    tracing::warn!("Failed to parse Meteora DLMM pool state, using placeholder (Paper mode)");
                    create_placeholder_swap_instructions()
                }
            } else {
                if ctx.trading_mode == TradingMode::Live {
                    tracing::error!("Failed to fetch pool account in Live mode");
                    return Err(anyhow!("Failed to fetch pool account in Live mode"));
                }
                tracing::warn!("Failed to fetch pool account, using placeholder (Paper mode)");
                create_placeholder_swap_instructions()
            }
        } else {
            if ctx.trading_mode == TradingMode::Live {
                tracing::error!("Unsupported DEX {} in Live mode", ctx.dex_program_id);
                return Err(anyhow!("Unsupported DEX {} in Live mode", ctx.dex_program_id));
            }
            tracing::warn!("Unsupported DEX {}, using placeholder (Paper mode)", ctx.dex_program_id);
            create_placeholder_swap_instructions()
        }
    } else {
        if ctx.trading_mode == TradingMode::Live {
            tracing::error!("No RPC client in Live mode");
            return Err(anyhow!("No RPC client in Live mode"));
        }
        tracing::warn!("No RPC client, using placeholder swap instructions (Paper mode)");
        create_placeholder_swap_instructions()
    };

    // Get recent blockhash (blocking call wrapped in spawn_blocking)
    let blockhash = if let Some(ref rpc_url) = ctx.rpc_url {
        let rpc_url_clone = rpc_url.clone();
        tokio::task::spawn_blocking(move || {
            let rpc = RpcClient::new(rpc_url_clone);
            rpc.get_latest_blockhash()
        }).await.ok().and_then(|r| r.ok()).unwrap_or(Hash::new_unique())
    } else {
        Hash::new_unique()
    };

    // Build complete unsigned transaction
    let (unsigned_tx, unsigned_tx_base64) = build_unsigned_transaction(
        swap_instructions,
        200_000,
        ctx.tier_params.emergency_exit.priority_fee_baseline as u64,
        &ctx.wallet,
        blockhash,
    )?;

    tracing::debug!("Built unsigned buy transaction: base64 len={}", unsigned_tx_base64.len());

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
                        let decoded_bytes = BASE64.decode(&signed_base64)
                            .map_err(|e| anyhow!("Failed to decode signed transaction base64: {}", e))?;
                        let decoded_tx = bincode::deserialize::<Transaction>(&decoded_bytes)
                            .map_err(|e| anyhow!("Failed to deserialize signed transaction: {}", e))?;
                        final_tx = decoded_tx;
                        tracing::info!("Signer client signed buy transaction successfully");
                    } else {
                        // Signer accepted but didn't return signed transaction
                        tracing::error!("Signer accepted but no signed transaction returned");
                        return Err(anyhow!("Signer accepted but no signed transaction returned"));
                    }
                } else {
                    let reason = resp.rejection_reason.unwrap_or_else(|| "Unknown".to_string());
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

    // Step 5: Submit transaction via Router
    if ctx.trading_mode == TradingMode::Live {
        if let Some(ref mut router) = ctx.router {
            tracing::info!("Submitting live buy transaction to Router pipeline");
            let priority_fee = ctx.tier_params.emergency_exit.priority_fee_baseline as u64;
            match router.submit_transaction(final_tx, priority_fee).await {
                Ok(SubmitResult::Confirmed { signature, slot }) => {
                    tracing::info!("Buy transaction confirmed on-chain: sig={}, slot={}", signature, slot);
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

    // Persist position with Opening status
    if let Some(ref repo) = ctx.positions_repo {
        if let Err(e) = repo.insert(&position).await {
            tracing::error!("Failed to persist new position {} to DB: {}", position.position_id, e);
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
        match lifecycle.transition(&position, PositionStatus::Open, tx_signature).await {
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
            tracing::error!("Failed to update position {} to Open status: {}", position.position_id, e);
        }
    }

    ctx.active_position = Some(position.clone());
    tracing::info!("Position created and stored in context: {}", position.position_id);

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
        ExitAction::TakeProfit { sell_fraction_of_remaining, .. } => (*sell_fraction_of_remaining, IntentType::Sell),
        ExitAction::TrailingStop { .. } => (1.0, IntentType::Sell),
        ExitAction::HardStopLoss => (1.0, IntentType::Emergency),
        ExitAction::Emergency { .. } => (1.0, IntentType::Emergency),
    };

    tracing::debug!("Building sell transaction for {:.1}% of remaining position", sell_fraction * 100.0);

    // Build unsigned sell transaction
    let mint_pubkey = Pubkey::from_str(&ctx.mint).unwrap_or_else(|_| Pubkey::new_unique());
    let pool_pubkey = Pubkey::from_str(&ctx.pool_address).unwrap_or_else(|_| Pubkey::new_unique());
    let dex_program_pubkey = Pubkey::from_str(&ctx.dex_program_id).unwrap_or_else(|_| Pubkey::new_unique());
    let amount_in_lamports = (position.size_sol * sell_fraction * 1_000_000_000.0) as u64;

    // Fetch pool state from RPC for Pump.fun
    let swap_instructions = if let Some(ref rpc_url) = ctx.rpc_url {
        if ctx.dex_program_id == pump_fun_builder::PUMP_FUN_PROGRAM.to_string() {
            let rpc_url_clone = rpc_url.clone();
            let pool_pubkey_clone = pool_pubkey;
            let pool_account = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_clone);
                rpc.get_account(&pool_pubkey_clone)
            }).await.ok().and_then(|r| r.ok());
            if let Some(account) = pool_account {
                if let Ok(curve) = pump_fun_builder::PumpFunBondingCurve::from_account_data(&account.data, &pool_pubkey) {
                    let expected_out = curve.calculate_sell_output(amount_in_lamports);
                    let minimum_amount_out = apply_slippage(expected_out, ctx.tier_params.emergency_exit.emergency_slippage_bps);
                    
                    let swap_params = SwapParams {
                        mint: mint_pubkey,
                        pool_address: pool_pubkey,
                        dex_program_id: dex_program_pubkey,
                        wallet: ctx.wallet,
                        direction: SwapDirection::Sell,
                        amount_in: amount_in_lamports,
                        minimum_amount_out,
                        slippage_bps: ctx.tier_params.emergency_exit.emergency_slippage_bps,
                    };
                    match pump_fun_builder::build_swap(&swap_params, &curve) {
                        Ok(ixs) => ixs,
                        Err(e) => {
                            tracing::error!("Failed to build Pump.fun sell: {}", e);
                            return Err(anyhow!("Tx builder error: {}", e));
                        }
                    }
                } else {
                    if ctx.trading_mode == TradingMode::Live {
                        tracing::error!("Failed to parse Pump.fun bonding curve in Live mode");
                        return Err(anyhow!("Failed to parse Pump.fun bonding curve in Live mode"));
                    }
                    tracing::warn!("Failed to parse Pump.fun bonding curve, using placeholder (Paper mode)");
                    create_placeholder_swap_instructions()
                }
            } else {
                if ctx.trading_mode == TradingMode::Live {
                    tracing::error!("Failed to fetch pool account in Live mode");
                    return Err(anyhow!("Failed to fetch pool account in Live mode"));
                }
                tracing::warn!("Failed to fetch pool account, using placeholder (Paper mode)");
                create_placeholder_swap_instructions()
            }
        } else if ctx.dex_program_id == raydium_amm_v4_builder::RAYDIUM_AMM_V4_PROGRAM.to_string() {
            let rpc_url_clone = rpc_url.clone();
            let pool_pubkey_clone = pool_pubkey;
            let pool_account = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_clone);
                rpc.get_account(&pool_pubkey_clone)
            }).await.ok().and_then(|r| r.ok());
            if let Some(account) = pool_account {
                if let Ok(mut pool_state) = raydium_amm_v4_builder::RaydiumAmmV4PoolState::from_account_data(&account.data) {
                    // Fetch vault balances for accurate pricing (fail-closed)
                    let vault_balances = fetch_vault_balances(
                        rpc_url.clone(),
                        pool_state.serum_coin_vault,
                        pool_state.serum_pc_vault,
                        "Raydium AMM v4",
                    ).await?;
                    
                    pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);
                    
                    let expected_out = pool_state.calculate_sell_output(amount_in_lamports);
                    ensure_nonzero_output(expected_out, "Raydium AMM v4", "sell")?;
                    let minimum_amount_out = apply_slippage(expected_out, ctx.tier_params.emergency_exit.emergency_slippage_bps);
                    
                    let swap_params = SwapParams {
                        mint: mint_pubkey,
                        pool_address: pool_pubkey,
                        dex_program_id: dex_program_pubkey,
                        wallet: ctx.wallet,
                        direction: SwapDirection::Sell,
                        amount_in: amount_in_lamports,
                        minimum_amount_out,
                        slippage_bps: ctx.tier_params.emergency_exit.emergency_slippage_bps,
                    };
                    match raydium_amm_v4_builder::build_swap(&swap_params, &pool_state) {
                        Ok(ixs) => ixs,
                        Err(e) => {
                            tracing::error!("Failed to build Raydium AMM v4 sell: {}", e);
                            return Err(anyhow!("Tx builder error: {}", e));
                        }
                    }
                } else {
                    if ctx.trading_mode == TradingMode::Live {
                        tracing::error!("Failed to parse Raydium AMM v4 pool state in Live mode");
                        return Err(anyhow!("Failed to parse Raydium AMM v4 pool state in Live mode"));
                    }
                    tracing::warn!("Failed to parse Raydium AMM v4 pool state, using placeholder (Paper mode)");
                    create_placeholder_swap_instructions()
                }
            } else {
                if ctx.trading_mode == TradingMode::Live {
                    tracing::error!("Failed to fetch pool account in Live mode");
                    return Err(anyhow!("Failed to fetch pool account in Live mode"));
                }
                tracing::warn!("Failed to fetch pool account, using placeholder (Paper mode)");
                create_placeholder_swap_instructions()
            }
        } else if ctx.dex_program_id == raydium_cpmm_builder::RAYDIUM_CPMM_PROGRAM.to_string() {
            let rpc_url_clone = rpc_url.clone();
            let pool_pubkey_clone = pool_pubkey;
            let pool_account = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_clone);
                rpc.get_account(&pool_pubkey_clone)
            }).await.ok().and_then(|r| r.ok());
            if let Some(account) = pool_account {
                if let Ok(mut pool_state) = raydium_cpmm_builder::CpmmPoolState::from_account_data(&account.data) {
                    // Fetch vault balances for accurate pricing (fail-closed)
                    let vault_balances = fetch_vault_balances(
                        rpc_url.clone(),
                        pool_state.token_0_vault,
                        pool_state.token_1_vault,
                        "Raydium CPMM",
                    ).await?;
                    
                    pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);
                    
                    let expected_out = pool_state.calculate_sell_output(amount_in_lamports);
                    ensure_nonzero_output(expected_out, "Raydium CPMM", "sell")?;
                    let minimum_amount_out = apply_slippage(expected_out, ctx.tier_params.emergency_exit.emergency_slippage_bps);
                    
                    let swap_params = SwapParams {
                        mint: mint_pubkey,
                        pool_address: pool_pubkey,
                        dex_program_id: dex_program_pubkey,
                        wallet: ctx.wallet,
                        direction: SwapDirection::Sell,
                        amount_in: amount_in_lamports,
                        minimum_amount_out,
                        slippage_bps: ctx.tier_params.emergency_exit.emergency_slippage_bps,
                    };
                    match raydium_cpmm_builder::build_swap(&swap_params, &pool_state) {
                        Ok(ixs) => ixs,
                        Err(e) => {
                            tracing::error!("Failed to build Raydium CPMM sell: {}", e);
                            return Err(anyhow!("Tx builder error: {}", e));
                        }
                    }
                } else {
                    if ctx.trading_mode == TradingMode::Live {
                        tracing::error!("Failed to parse Raydium CPMM pool state in Live mode");
                        return Err(anyhow!("Failed to parse Raydium CPMM pool state in Live mode"));
                    }
                    tracing::warn!("Failed to parse Raydium CPMM pool state, using placeholder (Paper mode)");
                    create_placeholder_swap_instructions()
                }
            } else {
                if ctx.trading_mode == TradingMode::Live {
                    tracing::error!("Failed to fetch pool account in Live mode");
                    return Err(anyhow!("Failed to fetch pool account in Live mode"));
                }
                tracing::warn!("Failed to fetch pool account, using placeholder (Paper mode)");
                create_placeholder_swap_instructions()
            }
        } else if ctx.dex_program_id == meteora_dlmm_builder::METEORA_DLMM_PROGRAM.to_string() {
            let rpc_url_clone = rpc_url.clone();
            let pool_pubkey_clone = pool_pubkey;
            let pool_account = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_clone);
                rpc.get_account(&pool_pubkey_clone)
            }).await.ok().and_then(|r| r.ok());
            if let Some(account) = pool_account {
                if let Ok(mut pool_state) = meteora_dlmm_builder::DlmmPoolState::from_account_data(&account.data, &pool_pubkey) {
                    // Fetch vault balances for accurate pricing (fail-closed)
                    let vault_balances = fetch_vault_balances(
                        rpc_url.clone(),
                        pool_state.reserve_x,
                        pool_state.reserve_y,
                        "Meteora DLMM",
                    ).await?;
                    
                    pool_state = pool_state.with_vault_balances(vault_balances.0, vault_balances.1);
                    
                    let expected_out = pool_state.calculate_sell_output(amount_in_lamports);
                    ensure_nonzero_output(expected_out, "Meteora DLMM", "sell")?;
                    let minimum_amount_out = apply_slippage(expected_out, ctx.tier_params.emergency_exit.emergency_slippage_bps);
                    
                    let swap_params = SwapParams {
                        mint: mint_pubkey,
                        pool_address: pool_pubkey,
                        dex_program_id: dex_program_pubkey,
                        wallet: ctx.wallet,
                        direction: SwapDirection::Sell,
                        amount_in: amount_in_lamports,
                        minimum_amount_out,
                        slippage_bps: ctx.tier_params.emergency_exit.emergency_slippage_bps,
                    };
                    match meteora_dlmm_builder::build_swap(&swap_params, &pool_state) {
                        Ok(ixs) => ixs,
                        Err(e) => {
                            tracing::error!("Failed to build Meteora DLMM sell: {}", e);
                            return Err(anyhow!("Tx builder error: {}", e));
                        }
                    }
                } else {
                    if ctx.trading_mode == TradingMode::Live {
                        tracing::error!("Failed to parse Meteora DLMM pool state in Live mode");
                        return Err(anyhow!("Failed to parse Meteora DLMM pool state in Live mode"));
                    }
                    tracing::warn!("Failed to parse Meteora DLMM pool state, using placeholder (Paper mode)");
                    create_placeholder_swap_instructions()
                }
            } else {
                if ctx.trading_mode == TradingMode::Live {
                    tracing::error!("Failed to fetch pool account in Live mode");
                    return Err(anyhow!("Failed to fetch pool account in Live mode"));
                }
                tracing::warn!("Failed to fetch pool account, using placeholder (Paper mode)");
                create_placeholder_swap_instructions()
            }
        } else {
            if ctx.trading_mode == TradingMode::Live {
                tracing::error!("Unsupported DEX {} in Live mode", ctx.dex_program_id);
                return Err(anyhow!("Unsupported DEX {} in Live mode", ctx.dex_program_id));
            }
            tracing::warn!("Unsupported DEX {}, using placeholder (Paper mode)", ctx.dex_program_id);
            create_placeholder_swap_instructions()
        }
    } else {
        if ctx.trading_mode == TradingMode::Live {
            tracing::error!("No RPC client in Live mode");
            return Err(anyhow!("No RPC client in Live mode"));
        }
        tracing::warn!("No RPC client, using placeholder swap instructions (Paper mode)");
        create_placeholder_swap_instructions()
    };

    // Get recent blockhash (blocking call wrapped in spawn_blocking)
    let blockhash = if let Some(ref rpc_url) = ctx.rpc_url {
        let rpc_url_clone = rpc_url.clone();
        tokio::task::spawn_blocking(move || {
            let rpc = RpcClient::new(rpc_url_clone);
            rpc.get_latest_blockhash()
        }).await.ok().and_then(|r| r.ok()).unwrap_or(Hash::new_unique())
    } else {
        Hash::new_unique()
    };

    // Build complete unsigned transaction
    let (unsigned_tx, unsigned_tx_base64) = build_unsigned_transaction(
        swap_instructions,
        200_000,
        ctx.tier_params.emergency_exit.priority_fee_baseline as u64 * 2,
        &ctx.wallet,
        blockhash,
    )?;

    let mut final_tx = unsigned_tx.clone();

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
                        let decoded_bytes = BASE64.decode(&signed_base64)
                            .map_err(|e| anyhow!("Failed to decode signed transaction base64: {}", e))?;
                        let decoded_tx = bincode::deserialize::<Transaction>(&decoded_bytes)
                            .map_err(|e| anyhow!("Failed to deserialize signed transaction: {}", e))?;
                        final_tx = decoded_tx;
                        tracing::info!("Signer client signed exit transaction successfully");
                    } else {
                        // Signer accepted but didn't return signed transaction
                        tracing::error!("Signer accepted but no signed transaction returned");
                        return Err(anyhow!("Signer accepted but no signed transaction returned"));
                    }
                } else {
                    let reason = resp.rejection_reason.unwrap_or_else(|| "Unknown".to_string());
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

// ... (rest of the code remains the same)
    if ctx.trading_mode == TradingMode::Live {
        if let Some(ref mut router) = ctx.router {
            tracing::info!("Submitting live sell transaction to Router pipeline");
            let priority_fee = ctx.tier_params.emergency_exit.priority_fee_baseline as u64 * 2;
            match router.submit_transaction(final_tx, priority_fee).await {
                Ok(SubmitResult::Confirmed { signature, slot }) => {
                    tracing::info!("Sell transaction confirmed on-chain: sig={}, slot={}", signature, slot);
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

    // Step 4: Update position state based on exit action using PositionLifecycle
    if let Some(ref mut pos) = ctx.active_position {
        let old_status = pos.status.clone();
        let (intermediate_status, final_status, tx_signature) = match &exit_action {
            ExitAction::TakeProfit { level, .. } => {
                match level {
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
                        pos.remaining_token_pct -= 0.20;
                        (PositionStatus::Tp3Filled, Some(PositionStatus::Closed), None)
                    }
                }
            }
            ExitAction::TrailingStop { .. } | ExitAction::HardStopLoss | ExitAction::Emergency { .. } => {
                pos.remaining_token_pct = 0.0;
                (PositionStatus::Closing, Some(PositionStatus::Closed), None)
            }
        };

        // Determine the target status for this transition
        let target_status = intermediate_status.clone();
        
        // Use PositionLifecycle for state transition validation and logging
        if let Some(ref lifecycle) = ctx.position_lifecycle {
            match lifecycle.transition(pos, target_status.clone(), tx_signature.clone()).await {
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
                            crate::state::position_lifecycle::Transition::OpenToClosing |
                            crate::state::position_lifecycle::Transition::Tp1ToClosing |
                            crate::state::position_lifecycle::Transition::Tp2ToClosing |
                            crate::state::position_lifecycle::Transition::Tp3ToClosing => {
                                if matches!(exit_action, ExitAction::TrailingStop { .. }) {
                                    Some(PositionLifecycleEvent::TrailingStopTriggered {
                                        position_id: pos.position_id.clone(),
                                        trigger_price_sol: current_price,
                                        high_water_sol: pos.trailing_high_water_sol.unwrap_or(current_price),
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
                                tracing::warn!("Unexpected transition in exit pipeline: {:?}", transition);
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
                        if let Ok(final_transition) = lifecycle.transition(pos, final_status_value.clone(), tx_signature.clone()).await {
                            pos.status = final_status_value;
                            tracing::info!("Position final lifecycle transition successful: {:?}", final_transition);
                            
                            // Record loss to circuit breaker if position closed with negative PnL
                            if pos.realized_pnl_net_sol < 0.0 {
                                if let Some(ref circuit_breaker) = ctx.circuit_breaker {
                                    let loss_pct = (-pos.realized_pnl_net_sol / ctx.capital_sol) * 100.0;
                                    let status = circuit_breaker.record_loss(ctx.tier, loss_pct);
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
                                if matches!(final_transition, crate::state::position_lifecycle::Transition::ClosingToClosed) {
                                    let duration = pos.closed_at_utc.unwrap_or(0) - pos.opened_at_utc;
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
                        let _ = lifecycle.transition_with_reason(
                            pos,
                            PositionStatus::StuckManualInterventionRequired,
                            format!("exit_pipeline_error: {}", e),
                            None,
                        ).await;
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
                    ExitAction::TakeProfit { level, .. } => format!("take_profit_{:?}", level).to_lowercase(),
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
    use solbot_common::types::position::{Position, PositionStatus};
    use solbot_common::types::tier::CapitalTier;
    use solbot_common::types::PositionLifecycleEvent;
    use std::sync::Arc;
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
            ExitAction::TakeProfit { level, .. } => {
                match level {
                    TpLevel::Tp1 => (PositionStatus::Tp1Filled, None),
                    TpLevel::Tp2 => (PositionStatus::Tp2Filled, None),
                    TpLevel::Tp3 => (PositionStatus::Tp3Filled, Some(PositionStatus::Closed)),
                }
            }
            _ => (PositionStatus::Closing, Some(PositionStatus::Closed)),
        };

        assert_eq!(intermediate_status, PositionStatus::Tp1Filled);
        assert!(final_status.is_none());

        let tp3_action = ExitAction::TakeProfit {
            level: TpLevel::Tp3,
            sell_fraction_of_remaining: 0.20,
        };

        let (intermediate_status, final_status) = match &tp3_action {
            ExitAction::TakeProfit { level, .. } => {
                match level {
                    TpLevel::Tp1 => (PositionStatus::Tp1Filled, None),
                    TpLevel::Tp2 => (PositionStatus::Tp2Filled, None),
                    TpLevel::Tp3 => (PositionStatus::Tp3Filled, Some(PositionStatus::Closed)),
                }
            }
            _ => (PositionStatus::Closing, Some(PositionStatus::Closed)),
        };

        assert_eq!(intermediate_status, PositionStatus::Tp3Filled);
        assert_eq!(final_status, Some(PositionStatus::Closed));

        let trailing_action = ExitAction::TrailingStop { 
            trigger_price: 1.8,
            high_water_mark: 2.0,
        };

        let (intermediate_status, final_status) = match &trailing_action {
            ExitAction::TrailingStop { .. } | ExitAction::HardStopLoss | ExitAction::Emergency { .. } => {
                (PositionStatus::Closing, Some(PositionStatus::Closed))
            }
            _ => (PositionStatus::Closing, Some(PositionStatus::Closed)),
        };

        assert_eq!(intermediate_status, PositionStatus::Closing);
        assert_eq!(final_status, Some(PositionStatus::Closed));
    }

    #[test]
    fn test_circuit_breaker_integration() {
        let circuit_breaker = Arc::new(CircuitBreaker::new());
        
        // Test recording a loss
        let status = circuit_breaker.record_loss(CapitalTier::Micro, 10.0);
        assert_eq!(status.daily_loss_pct, 10.0);
        assert!(!status.warning_triggered);
        assert!(!status.limit_triggered);
        
        // Test warning threshold
        let status = circuit_breaker.record_loss(CapitalTier::Micro, 5.0);
        assert_eq!(status.daily_loss_pct, 15.0);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);
        
        // Test limit threshold
        let status = circuit_breaker.record_loss(CapitalTier::Micro, 10.0);
        assert_eq!(status.daily_loss_pct, 25.0);
        assert!(status.warning_triggered);
        assert!(status.limit_triggered);
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let circuit_breaker = Arc::new(CircuitBreaker::new());
        
        // Record some losses
        circuit_breaker.record_loss(CapitalTier::Micro, 20.0);
        let status = circuit_breaker.get_status(CapitalTier::Micro);
        assert_eq!(status.daily_loss_pct, 20.0);
        
        // Reset
        circuit_breaker.reset_daily();
        let status = circuit_breaker.get_status(CapitalTier::Micro);
        assert_eq!(status.daily_loss_pct, 0.0);
        assert!(!status.warning_triggered);
        assert!(!status.limit_triggered);
    }

    #[test]
    fn test_circuit_breaker_tier_specific_thresholds() {
        let circuit_breaker = Arc::new(CircuitBreaker::new());
        
        // Micro tier: warning 15%, limit 25%
        let status = circuit_breaker.record_loss(CapitalTier::Micro, 15.0);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);
        
        circuit_breaker.reset_daily();
        
        // Small tier: warning 16%, limit 24%
        let status = circuit_breaker.record_loss(CapitalTier::Small, 16.0);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);
        
        circuit_breaker.reset_daily();
        
        // Medium tier: warning 15%, limit 21.6%
        let status = circuit_breaker.record_loss(CapitalTier::Medium, 15.0);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);
        
        circuit_breaker.reset_daily();
        
        // Large tier: warning 12%, limit 16.6%
        let status = circuit_breaker.record_loss(CapitalTier::Large, 12.0);
        assert!(status.warning_triggered);
        assert!(!status.limit_triggered);
    }
}
