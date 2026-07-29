use super::{market_data_lane::MarketDataLane, ActorConfig};
use solbot_common::types::{ControlEvent, MarketDataEvent};
use std::time::Duration;

pub struct ActorContext {
    pub mint: String,
    pub pool_address: String,
}

pub async fn run(
    mint: String,
    mut control_rx: tokio::sync::mpsc::Receiver<ControlEvent>,
    mut market_data_rx: MarketDataLane,
    ctx: ActorContext,
    config: ActorConfig,
) {
    tracing::info!("Starting mint actor for {}", mint);

    loop {
        tokio::select! {
            biased;
            control_event = control_rx.recv() => {
                match control_event {
                    Some(event) => {
                        let start = std::time::Instant::now();
                        tracing::debug!("Actor {} received control event: {:?}", mint, event);
                        
                        process_control_event(&mint, event, &ctx).await;
                        
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
                        
                        process_market_data_event(&mint, event, &ctx).await;
                        
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

async fn process_control_event(mint: &str, event: ControlEvent, _ctx: &ActorContext) {
    match event {
        ControlEvent::MigrationEvent { mint: _, new_pool_address } => {
            tracing::info!("Migration event for {}: new pool = {}", mint, new_pool_address);
        }
        ControlEvent::KillSwitch { activated, reason } => {
            tracing::warn!("Kill switch for {}: activated={}, reason={}", mint, activated, reason);
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

async fn process_market_data_event(mint: &str, event: MarketDataEvent, _ctx: &ActorContext) {
    match event {
        MarketDataEvent::PriceTick { mint: _, pool_address, price_sol, timestamp } => {
            tracing::trace!(
                "Price tick for {}: pool={}, price={} SOL, ts={}",
                mint,
                pool_address,
                price_sol,
                timestamp
            );
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
