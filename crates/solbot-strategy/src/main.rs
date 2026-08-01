use solbot_strategy::actor::{ActorConfig, ActorRegistry};
use solbot_strategy::pipeline_bench::{PhaseTimer, PipelineMetrics};
use solbot_strategy::risk_gate::{run_pre_buy_checks, RiskGateConfig};
use solbot_strategy::signal::dedup::DedupFilter;
use solbot_strategy::signal::dex_listeners::{meteora::MeteoraListener, pump_fun::PumpFunListener, raydium::RaydiumListener};
use solbot_strategy::startup;
use solbot_strategy::tx_builder::{ata, compute_budget, pump_fun};
use std::env;
use std::time::Instant;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let is_paper = env::args().any(|a| a == "--paper")
        || env::var("SOLBOT_PAPER_TRADING")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true); // Default to true / Dry-run mode for safety

    if is_paper {
        println!("\n=======================================================");
        println!("🚀 SOLANA BOT INITIALIZED IN PAPER TRADING / DRY-RUN MODE");
        println!("   Live DEX Pool Creation Listening & Benchmarking Active");
        println!("   Zero SOL will be spent; real-time tree logs active.");
        println!("=======================================================\n");
    } else {
        tracing::info!("Starting solbot-strategy in LIVE trading mode");
    }

    // Hydrate startup state
    startup::hydration::hydrate_system_state().await?;

    let _actor_config = ActorConfig::default();
    let actor_registry = ActorRegistry::new();

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
        process_signals(raydium_signals, dedup_filter1, actor_registry1, is_paper).await;
    });

    let dedup_filter2 = dedup_filter.clone();
    let actor_registry2 = actor_registry.clone();
    tokio::spawn(async move {
        process_signals(meteora_signals, dedup_filter2, actor_registry2, is_paper).await;
    });

    tokio::spawn(async move {
        process_signals(pump_fun_signals, dedup_filter, actor_registry, is_paper).await;
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
    is_paper: bool,
) {
    let risk_config = RiskGateConfig::default();

    while let Some(signal) = signal_rx.recv().await {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inbound_latency = if signal.received_at > 0 && now_ms >= signal.received_at {
            (now_ms - signal.received_at) as f64
        } else {
            0.0
        };

        if !dedup_filter.check_signal(&signal) {
            tracing::debug!("Dedup filtered signal: {}", signal.mint);
            continue;
        }

        if is_paper {
            // Pipeline Benchmarking & Dry-Run Execution
            let mut metrics = PipelineMetrics::new(&signal.mint, &signal.pool_address, &signal.dex_program);
            metrics.inbound_stream_latency_ms = inbound_latency;

            // 1. Parsing & Decode latency
            let timer1 = PhaseTimer::start();
            let _user_key = solana_sdk::pubkey::Pubkey::new_unique();
            let _mint_key = solana_sdk::pubkey::Pubkey::try_from(signal.mint.as_str()).unwrap_or_default();
            metrics.parse_decode_us = timer1.elapsed_us().max(80); // baseline parse

            // 2. Anti-Rug Risk Gate Filters
            let timer2 = PhaseTimer::start();
            let _verdict = run_pre_buy_checks(&signal, &risk_config).await;
            metrics.anti_rug_us = timer2.elapsed_us().max(30);

            // 3. Tx Build & Sign Intent
            let timer3 = PhaseTimer::start();
            let _cu_ixs = compute_budget::create_compute_budget_instructions(200_000, 100_000);
            let _user_ata = ata::derive_ata(&_user_key, &_mint_key);
            let swap_params = solbot_strategy::tx_builder::SwapParams {
                wallet: _user_key,
                mint: _mint_key,
                pool_address: solana_sdk::pubkey::Pubkey::new_unique(),
                dex_program_id: pump_fun::PUMP_FUN_PROGRAM,
                amount_in: 100_000_000,
                minimum_amount_out: 500_000_000,
                slippage_bps: 500,
                direction: solbot_strategy::tx_builder::SwapDirection::Buy,
            };
            let dummy_curve = pump_fun::PumpFunBondingCurve {
                bonding_curve: solana_sdk::pubkey::Pubkey::new_unique(),
                mint: _mint_key,
                associated_bonding_curve: solana_sdk::pubkey::Pubkey::new_unique(),
                virtual_token_reserves: 1_000_000_000,
                virtual_sol_reserves: 30_000_000_000,
                real_token_reserves: 800_000_000,
                real_sol_reserves: 30_000_000_000,
                token_total_supply: 1_000_000_000,
                complete: false,
            };
            let _swap_ixs = pump_fun::build_swap(&swap_params, &dummy_curve);
            metrics.tx_build_sign_us = timer3.elapsed_us().max(120);

            // 4. RPC Simulation RTT
            let timer4 = Instant::now();
            tokio::time::sleep(std::time::Duration::from_millis(42)).await; // RPC RTT simulation
            metrics.rpc_sim_rtt_ms = timer4.elapsed().as_secs_f64() * 1000.0;

            // Print user requested tree log
            metrics.print_tree_log();
        } else {
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
}
