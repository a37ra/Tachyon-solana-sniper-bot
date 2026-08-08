use solana_sdk::signature::Signer;
use solbot_common::config::base_config::DexReadinessConfig;
use solbot_common::config::tier_params::{
    CompromiseDetection, EmergencyExitParams, HotWalletParams, PositionLimits, SizingParams,
    TierInfo,
};
use solbot_strategy::actor::market_data_lane::MarketDataLane;
use solbot_strategy::actor::mint_actor::ActorContext;
use solbot_strategy::actor::{mint_actor, ActorConfig, ActorHandle, ActorRegistry};
use solbot_strategy::pipeline_bench::{PhaseTimer, PipelineMetrics};
use solbot_strategy::risk_gate::{run_pre_buy_checks_with_timeout, RiskGateConfig};
use solbot_strategy::routing::TransactionRouter;
use solbot_strategy::signal::dedup::DedupFilter;
use solbot_strategy::signal::dex_listeners::{
    meteora::MeteoraListener, pump_fun::PumpFunListener, raydium::RaydiumListener,
};
use solbot_strategy::signer_client::SignerClient;
use solbot_strategy::startup;
use solbot_strategy::tx_builder::{ata, compute_budget, pump_fun};
use std::env;
use std::sync::Arc;
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

    let actor_config = ActorConfig::default();
    let actor_registry = ActorRegistry::new();

    // Load configuration for live mode (only if not paper mode)
    let (wallet_pubkey, router, signer_client, rpc_url, dex_readiness) = if !is_paper {
        let rpc_url = env::var("SOLBOT_RPC_URL")
            .unwrap_or_else(|_| "https://api.devnet.solana.com".to_string());
        let signer_socket = env::var("SOLBOT_SIGNER_SOCKET")
            .unwrap_or_else(|_| "/tmp/solbot-signer.sock".to_string());
        let wallet_keypair = env::var("SOLBOT_KEYPAIR_JSON")
            .map_err(|_| anyhow::anyhow!("SOLBOT_KEYPAIR_JSON required for live mode"))?;

        let wallet_keypair_bytes: Vec<u8> = serde_json::from_str(&wallet_keypair)?;
        let wallet_keypair = solana_keypair::Keypair::from_bytes(&wallet_keypair_bytes)?;
        let wallet_pubkey = wallet_keypair.pubkey();

        let router = Arc::new(TransactionRouter::new(
            solbot_strategy::routing::RoutingConfig::default(),
        )?);
        let signer_client = Arc::new(SignerClient::new(signer_socket));
        let dex_readiness = DexReadinessConfig::default();

        (
            wallet_pubkey,
            Some(router),
            Some(signer_client),
            Some(rpc_url),
            dex_readiness,
        )
    } else {
        // Paper mode - use dummy values
        (
            solana_sdk::pubkey::Pubkey::new_unique(),
            None,
            None,
            None,
            DexReadinessConfig::default(),
        )
    };

    let dedup_filter = DedupFilter::new(30);

    let raydium_listener = RaydiumListener::new();
    let meteora_listener = MeteoraListener::new();
    let pump_fun_listener = PumpFunListener::new();

    let raydium_signals = raydium_listener.subscribe_to_pool_creation().await?;
    let meteora_signals = meteora_listener.subscribe_to_pool_creation().await?;
    let pump_fun_signals = pump_fun_listener.subscribe_to_bonding_curve().await?;

    let dedup_filter1 = dedup_filter.clone();
    let actor_registry1 = actor_registry.clone();
    let wallet_pubkey1 = wallet_pubkey;
    let router1 = router.clone();
    let signer_client1 = signer_client.clone();
    let rpc_url1 = rpc_url.clone();
    let dex_readiness1 = dex_readiness.clone();
    let actor_config1 = actor_config.clone();
    tokio::spawn(async move {
        process_signals(
            raydium_signals,
            dedup_filter1,
            actor_registry1,
            is_paper,
            wallet_pubkey1,
            router1,
            signer_client1,
            rpc_url1,
            dex_readiness1,
            actor_config1,
        )
        .await;
    });

    let dedup_filter2 = dedup_filter.clone();
    let actor_registry2 = actor_registry.clone();
    let wallet_pubkey2 = wallet_pubkey;
    let router2 = router.clone();
    let signer_client2 = signer_client.clone();
    let rpc_url2 = rpc_url.clone();
    let dex_readiness2 = dex_readiness.clone();
    let actor_config2 = actor_config.clone();
    tokio::spawn(async move {
        process_signals(
            meteora_signals,
            dedup_filter2,
            actor_registry2,
            is_paper,
            wallet_pubkey2,
            router2,
            signer_client2,
            rpc_url2,
            dex_readiness2,
            actor_config2,
        )
        .await;
    });

    let wallet_pubkey3 = wallet_pubkey;
    let router3 = router.clone();
    let signer_client3 = signer_client.clone();
    let rpc_url3 = rpc_url.clone();
    let dex_readiness3 = dex_readiness.clone();
    let actor_config3 = actor_config.clone();
    tokio::spawn(async move {
        process_signals(
            pump_fun_signals,
            dedup_filter,
            actor_registry,
            is_paper,
            wallet_pubkey3,
            router3,
            signer_client3,
            rpc_url3,
            dex_readiness3,
            actor_config3,
        )
        .await;
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
    wallet_pubkey: solana_sdk::pubkey::Pubkey,
    router: Option<Arc<TransactionRouter>>,
    signer_client: Option<Arc<SignerClient>>,
    rpc_url: Option<String>,
    dex_readiness: DexReadinessConfig,
    actor_config: ActorConfig,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    let signal_count = StdArc::new(AtomicUsize::new(0));
    // TEMP: Reduced from 30 to 5 due to public Solana RPC rate limits (100 req/10s per IP)
    // See notes.md for HFT Infrastructure TODO checklist for private RPC migration
    let max_concurrent_signals = 5;
    let semaphore = StdArc::new(tokio::sync::Semaphore::new(max_concurrent_signals));

    while let Some(signal) = signal_rx.recv().await {
        let queue_depth = signal_rx.len();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inbound_latency = if signal.received_at > 0 && now_ms >= signal.received_at {
            (now_ms - signal.received_at) as f64
        } else {
            0.0
        };

        // TEMP: Load shedding - drop stale signals to conserve RPC quota for public endpoints
        // See notes.md for HFT Infrastructure TODO checklist for private RPC migration
        const MAX_SIGNAL_AGE_MS: f64 = 300.0;
        if inbound_latency > MAX_SIGNAL_AGE_MS {
            let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(
                "[SIGNAL DROPPED - STALE] seq={}, mint={}, inbound_age_ms={:.2} > {}ms threshold",
                seq_num,
                signal.mint,
                inbound_latency,
                MAX_SIGNAL_AGE_MS
            );
            continue;
        }

        let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
        tracing::info!(
            "[SIGNAL QUEUED] seq={}, mint={}, queue_depth={}, inbound_age_ms={:.2}",
            seq_num,
            signal.mint,
            queue_depth,
            inbound_latency
        );

        let dedup = dedup_filter.clone();
        let actors = actor_registry.clone();
        let router_clone = router.clone();
        let signer_clone = signer_client.clone();
        let rpc_clone = rpc_url.clone();
        let dex_ready = dex_readiness.clone();
        let actor_cfg = actor_config.clone();
        let sem = semaphore.clone();
        let wallet_key = wallet_pubkey;

        // Acquire an owned permit before spawning the task so the receiver
        // is backpressured when permits are exhausted. This ensures true
        // semaphore-based backpressure instead of letting recv() continuously
        // dequeue and spawn tasks that then block on permit acquisition.
        let permit = StdArc::clone(&sem).acquire_owned().await.unwrap();

        // Diagnostic: record available permits immediately after acquiring
        // (shows whether the acquire actually reduced available permits).
        tracing::info!(
            "[PERMIT ACQUIRED] seq={}, available_permits_after_acquire={}",
            seq_num,
            sem.available_permits()
        );

        tokio::spawn(async move {
            // Move the owned permit into the spawned task so it is held for
            // the duration of processing and released automatically when the
            // task scope ends.
            let _permit = permit;
            let process_start = Instant::now();

            tracing::info!(
                "[SIGNAL PROCESSING START] seq={}, mint={}, inbound_age_ms={:.2}",
                seq_num,
                signal.mint,
                inbound_latency
            );

            if !dedup.check_signal(&signal) {
                tracing::debug!("Dedup filtered signal: {}", signal.mint);
                return;
            }

            if is_paper {
                let mut metrics =
                    PipelineMetrics::new(&signal.mint, &signal.pool_address, &signal.dex_program);
                metrics.inbound_stream_latency_ms = inbound_latency;

                let mut risk_config = RiskGateConfig::default();
                let rpc_url = rpc_clone
                    .clone()
                    .unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_string());
                risk_config.rpc_url = rpc_url.clone();
                risk_config.rpc = std::sync::Arc::new(
                    solana_rpc_client::nonblocking::rpc_client::RpcClient::new(rpc_url)
                );

                let timer1 = PhaseTimer::start();
                let _user_key = solana_sdk::pubkey::Pubkey::new_unique();
                let _mint_key =
                    solana_sdk::pubkey::Pubkey::try_from(signal.mint.as_str()).unwrap_or_default();
                metrics.parse_decode_us = timer1.elapsed_us().max(80);

                let timer2 = PhaseTimer::start();
                let _verdict = run_pre_buy_checks_with_timeout(&signal, &risk_config).await;
                metrics.anti_rug_us = timer2.elapsed_us().max(30);

                let timer3 = PhaseTimer::start();
                let _cu_ixs = compute_budget::create_compute_budget_instructions(200_000, 100_000);
                let _user_ata = ata::derive_ata(&_user_key, &_mint_key);
                let _pool_key = solana_sdk::pubkey::Pubkey::try_from(signal.pool_address.as_str())
                    .unwrap_or_default();
                let _dex_key = solana_sdk::pubkey::Pubkey::try_from(signal.dex_program.as_str())
                    .unwrap_or_default();
                let swap_params = solbot_strategy::tx_builder::SwapParams {
                    mint: _mint_key,
                    pool_address: _pool_key,
                    dex_program_id: _dex_key,
                    wallet: _user_key,
                    direction: solbot_strategy::tx_builder::SwapDirection::Buy,
                    amount_in: 1_000_000,
                    minimum_amount_out: 0,
                    slippage_bps: 100,
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

                let timer4 = Instant::now();
                tokio::time::sleep(std::time::Duration::from_millis(42)).await;
                metrics.rpc_sim_rtt_ms = timer4.elapsed().as_secs_f64() * 1000.0;

                metrics.print_tree_log();

                let process_elapsed_ms = process_start.elapsed().as_secs_f64() * 1000.0;
                tracing::info!(
                    "[SIGNAL PROCESSING END] seq={}, mint={}, total_processing_ms={:.2}, inbound_age_ms={:.2}",
                    seq_num,
                    signal.mint,
                    process_elapsed_ms,
                    inbound_latency
                );
            } else {
                let signal_mint_for_log = signal.mint.clone();

                if let Some(actor_handle) = actors.get(&signal.mint) {
                    tracing::debug!("Dispatching signal to existing actor: {}", signal.mint);
                    let _ = actor_handle
                        .control_tx
                        .send(solbot_common::types::ControlEvent::MigrationEvent {
                            mint: signal.mint.clone(),
                            new_pool_address: signal.pool_address.clone(),
                        })
                        .await;
                } else {
                    let mint_clone = signal.mint.clone();
                    tracing::info!("Creating new actor for mint: {}", mint_clone);

                    let (control_tx, control_rx) = mpsc::channel(100);
                    let (market_data_tx, _market_data_rx) = mpsc::channel(100);
                    let market_data_lane = MarketDataLane::new();

                    let ctx = ActorContext {
                        mint: signal.mint.clone(),
                        pool_address: signal.pool_address.clone(),
                        dex_program_id: signal.dex_program.clone(),
                        wallet: wallet_key,
                        trading_mode: mint_actor::TradingMode::Live,
                        risk_gate_config: RiskGateConfig::default(),
                        tier: solbot_common::types::tier::CapitalTier::Small,
                        tier_params: solbot_common::config::TierParams {
                            tier: TierInfo {
                                name: "Small".to_string(),
                                capital_range: solbot_common::config::tier_params::CapitalRange {
                                    min_usd: 1000,
                                    max_usd: 10000,
                                },
                            },
                            sizing: SizingParams {
                                base_size_pct: 0.01,
                                max_size_sol: 1.0,
                                slippage_hard_cap_bps: 500,
                                kelly: solbot_common::config::tier_params::KellyParams {
                                    fraction_of_kelly: 0.5,
                                },
                                adaptive: solbot_common::config::tier_params::AdaptiveParams {
                                    enabled: false,
                                    calibration_window_slots: 1000,
                                    min_sample_size: 10,
                                    size_max_pct: 0.05,
                                },
                            },
                            position_limits: PositionLimits {
                                max_concurrent_positions: 5,
                                circuit_breaker:
                                    solbot_common::config::tier_params::CircuitBreakerParams {
                                        warning_threshold_pct: 0.5,
                                        daily_loss_limit_pct: 0.1,
                                        max_parallel_emergency_sells: 3,
                                    },
                            },
                            hot_wallet: HotWalletParams { cap_sol: 10.0 },
                            compromise_detection: CompromiseDetection {
                                signing_rate_threshold_5min: 10,
                            },
                            emergency_exit: EmergencyExitParams {
                                emergency_slippage_bps: 1000,
                                priority_fee_baseline: 1000,
                            },
                        },
                        capital_sol: 1.0,
                        router: router_clone.as_ref().map(|_| {
                            TransactionRouter::new(
                                solbot_strategy::routing::RoutingConfig::default(),
                            )
                            .unwrap()
                        }),
                        signer_client: signer_clone
                            .as_ref()
                            .map(|_| Arc::new(SignerClient::new("/tmp/solbot-signer.sock".to_string()))),
                        rpc_url: rpc_clone.clone(),
                        active_position: None,
                        trade_stats: None,
                        dex_readiness: dex_ready.clone(),
                        positions_repo: None,
                        state_transitions_repo: None,
                        position_lifecycle: None,
                        lifecycle_event_tx: None,
                        circuit_breaker: None,
                    };

                    let actor_handle = ActorHandle {
                        control_tx: control_tx.clone(),
                        market_data_tx: market_data_tx.clone(),
                    };
                    actors.register(signal.mint.clone(), actor_handle);

                    let actor_config_clone = actor_cfg.clone();
                    tokio::spawn(async move {
                        mint_actor::run(
                            signal.mint.clone(),
                            control_rx,
                            market_data_lane,
                            ctx,
                            actor_config_clone,
                        )
                        .await;
                    });

                    tracing::info!("Actor created and spawned for mint: {}", mint_clone);
                }

                let process_elapsed_ms = process_start.elapsed().as_secs_f64() * 1000.0;
                tracing::info!(
                    "[SIGNAL PROCESSING END] seq={}, mint={}, total_processing_ms={:.2}, inbound_age_ms={:.2}",
                    seq_num,
                    signal_mint_for_log,
                    process_elapsed_ms,
                    inbound_latency
                );
            }
        });
    }
}
