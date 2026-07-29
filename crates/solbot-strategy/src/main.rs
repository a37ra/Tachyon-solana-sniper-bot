pub mod actor;
pub mod circuit_breaker;
pub mod exit;
pub mod risk_gate;
pub mod routing;
pub mod signal;
pub mod signer_client;
pub mod sizing;
pub mod smart_money;
pub mod startup;
pub mod state;
pub mod wsol;
pub mod pnl;
pub mod tx_builder;
use actor::{ActorConfig, ActorRegistry};
use signal::dedup::DedupFilter;
use signal::dex_listeners::{meteora::MeteoraListener, pump_fun::PumpFunListener, raydium::RaydiumListener};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    
    tracing::info!("Starting solbot-strategy");
    
    // Hydrate startup state
    startup::hydration::hydrate_system_state().await?;
    
    let _actor_config = ActorConfig::default();
    let actor_registry = ActorRegistry::new();
    let _circuit_breaker = circuit_breaker::CircuitBreaker::new();
    
    let (_control_tx, _control_rx) = mpsc::channel::<solbot_common::types::ControlEvent>(64);
    let dedup_filter = DedupFilter::new(30);
    
    let raydium_listener = RaydiumListener::new();
    let meteora_listener = MeteoraListener::new();
    let pump_fun_listener = PumpFunListener::new();
    
    let raydium_signals = raydium_listener.subscribe_to_pool_creation().await?;
    let meteora_signals = meteora_listener.subscribe_to_pool_creation().await?;
    let pump_fun_signals = pump_fun_listener.subscribe_to_bonding_curve().await?;
    
    let dedup_filter1 = dedup_filter.clone();
    let actor_registry1 = actor_registry.clone();
    tokio::spawn(async move {
        process_signals(raydium_signals, dedup_filter1, actor_registry1).await;
    });
    
    let dedup_filter2 = dedup_filter.clone();
    let actor_registry2 = actor_registry.clone();
    tokio::spawn(async move {
        process_signals(meteora_signals, dedup_filter2, actor_registry2).await;
    });
    
    tokio::spawn(async move {
        process_signals(pump_fun_signals, dedup_filter, actor_registry).await;
    });
    
    tracing::info!("Actor system & Strategy services initialized");
    
    tokio::signal::ctrl_c().await?;
    tracing::info!("Received shutdown signal");
    
    Ok(())
}

async fn process_signals(
    mut signal_rx: mpsc::Receiver<solbot_common::types::Signal>,
    dedup_filter: DedupFilter,
    actor_registry: ActorRegistry,
) {
    while let Some(signal) = signal_rx.recv().await {
        if !dedup_filter.check_signal(&signal) {
            tracing::debug!("Dedup filtered signal: {}", signal.mint);
            continue;
        }
        
        if let Some(actor_handle) = actor_registry.get(&signal.mint) {
            tracing::debug!("Dispatching signal to existing actor: {}", signal.mint);
            let _ = actor_handle.control_tx.send(solbot_common::types::ControlEvent::MigrationEvent {
                mint: signal.mint.clone(),
                new_pool_address: signal.pool_address.clone(),
            }).await;
        } else {
            tracing::info!("No actor found for mint {}, would create new actor", signal.mint);
        }
    }
}
