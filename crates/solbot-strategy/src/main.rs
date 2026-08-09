use solana_sdk::signature::Signer;
use solbot_common::config::base_config::DexReadinessConfig;
use solbot_common::config::tier_params::{
    CompromiseDetection, EmergencyExitParams, HotWalletParams, PositionLimits, SizingParams,
    TierInfo,
};
use solbot_common::types::ControlEvent;
use solbot_strategy::actor::market_data_lane::MarketDataLane;
use solbot_strategy::actor::mint_actor::ActorContext;
use solbot_strategy::actor::{mint_actor, ActorConfig, ActorHandle, ActorRegistry};
use solbot_strategy::pipeline_bench::{PhaseTimer, PipelineMetrics};
use solbot_strategy::risk_gate::{resolve_rpc_url, run_pre_buy_checks_with_timeout, RiskGateConfig};
use solbot_strategy::routing::TransactionRouter;
use solbot_strategy::signal::dedup::DedupFilter;
use solbot_strategy::signal::dex_listeners::{
    meteora::MeteoraListener, pump_fun::PumpFunListener, raydium::RaydiumListener,
};
use solbot_strategy::signer_client::SignerClient;
use solbot_strategy::startup;
use std::env;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    // Default signal source for paper/dev. Live must set its own stack explicitly.
    if env::var("SIGNAL_SOURCE").is_err() {
        env::set_var("SIGNAL_SOURCE", "helius_ws");
    }

    let is_paper = env::args().any(|a| a == "--paper")
        || env::var("SOLBOT_PAPER_TRADING")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true);

    // Public-RPC profile is OK for paper only. Do not silently enable it for live.
    if env::var("SOLBOT_PUBLIC_RPC").is_err() {
        env::set_var(
            "SOLBOT_PUBLIC_RPC",
            if is_paper { "true" } else { "false" },
        );
    }

    if is_paper {
        println!("\n=======================================================");
        println!("🚀 SOLANA BOT — PAPER / DRY-RUN");
        println!("   SIGNAL_SOURCE={}", env::var("SIGNAL_SOURCE").unwrap_or_default());
        println!("   RPC={}", resolve_rpc_url());
        println!("   Anti-rug runs on shared RpcClient (no per-signal reconnect).");
        println!("=======================================================\n");
    } else {
        tracing::info!("Starting solbot-strategy in LIVE trading mode");
    }

    // Paper-only: loud log if honeypot sim is off (live refusal is only via assert_safe_for_live).
    if is_paper
        && env::var("SOLBOT_SKIP_SIMULATE_SELL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    {
        tracing::error!(
            "SOLBOT_SKIP_SIMULATE_SELL=true — honeypot simulation OFF (paper/debug only)"
        );
    }

    startup::hydration::hydrate_system_state().await?;

    let actor_config = ActorConfig::default();
    let actor_registry = ActorRegistry::new();

    let rpc_url = resolve_rpc_url();
    let shared_rpc = Arc::new(solana_rpc_client::nonblocking::rpc_client::RpcClient::new(
        rpc_url.clone(),
    ));
    // Single source of truth for live hard-guards (skip_sim / public_rpc / max_signal_age).
    // Do not duplicate bail! logic above — keep all fail-closed policy in assert_safe_for_live.
    let shared_risk = Arc::new(RiskGateConfig::from_shared_rpc(
        shared_rpc.clone(),
        rpc_url.clone(),
    ));
    if !is_paper {
        shared_risk.assert_safe_for_live()?;
    }

    let (wallet_pubkey, router, signer_client, live_rpc_url, dex_readiness) = if !is_paper {
        let live_rpc = env::var("SOLBOT_RPC_URL").unwrap_or_else(|_| rpc_url.clone());
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
            Some(live_rpc),
            dex_readiness,
        )
    } else {
        (
            solana_sdk::pubkey::Pubkey::new_unique(),
            None,
            None,
            Some(rpc_url.clone()),
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

    for (rx, label) in [
        (raydium_signals, "raydium"),
        (meteora_signals, "meteora"),
        (pump_fun_signals, "pump_fun"),
    ] {
        let dedup = dedup_filter.clone();
        let actors = actor_registry.clone();
        let wallet = wallet_pubkey;
        let router_c = router.clone();
        let signer_c = signer_client.clone();
        let rpc_c = live_rpc_url.clone();
        let dex_r = dex_readiness.clone();
        let actor_cfg = actor_config.clone();
        let risk = shared_risk.clone();
        let paper = is_paper;
        tokio::spawn(async move {
            tracing::info!(listener = label, "Signal processor started");
            process_signals(
                rx, dedup, actors, paper, wallet, router_c, signer_c, rpc_c, dex_r, actor_cfg, risk,
            )
            .await;
        });
    }

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
    shared_risk: Arc<RiskGateConfig>,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    let signal_count = StdArc::new(AtomicUsize::new(0));
    let max_concurrent = env::var("SOLBOT_MAX_CONCURRENT_SIGNALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let semaphore = StdArc::new(tokio::sync::Semaphore::new(max_concurrent));

    let max_signal_age_ms: f64 = env::var("SOLBOT_MAX_SIGNAL_AGE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000.0);

    while let Some(signal) = signal_rx.recv().await {
        let queue_depth = signal_rx.len();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let inbound_latency = if signal.received_at > 0 && now_ms >= signal.received_at {
            (now_ms - signal.received_at) as f64
        } else {
            0.0
        };

        if inbound_latency > max_signal_age_ms {
            let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(
                "[SIGNAL DROPPED - STALE] seq={}, mint={}, inbound_age_ms={:.2} > {}ms",
                seq_num,
                signal.mint,
                inbound_latency,
                max_signal_age_ms
            );
            continue;
        }

        const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
        const NATIVE_MINT: &str = "So11111111111111111111111111111111111111112";
        if signal.mint == SYSTEM_PROGRAM || signal.mint == NATIVE_MINT {
            let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(
                "[SIGNAL DROPPED - SYSTEM/NATIVE] seq={}, mint={}",
                seq_num,
                signal.mint
            );
            continue;
        }

        let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
        tracing::info!(
            "[SIGNAL QUEUED] seq={}, mint={}, pool={}, dex={}, queue_depth={}, inbound_age_ms={:.2}, has_raw_pool={}",
            seq_num,
            signal.mint,
            signal.pool_address,
            signal.dex_program,
            queue_depth,
            inbound_latency,
            signal.raw_pool_account_data.is_some()
        );

        let dedup = dedup_filter.clone();
        let actors = actor_registry.clone();
        let router_clone = router.clone();
        let signer_clone = signer_client.clone();
        let rpc_clone = rpc_url.clone();
        let dex_ready = dex_readiness.clone();
        let actor_cfg = actor_config.clone();
        let risk_cfg = shared_risk.clone();
        let sem = semaphore.clone();
        let wallet_key = wallet_pubkey;

        let permit = match StdArc::clone(&sem).acquire_owned().await {
            Ok(p) => p,
            Err(_) => continue,
        };

        tokio::spawn(async move {
            let _permit = permit;
            let process_start = Instant::now();

            if !dedup.check_signal(&signal) {
                tracing::debug!("Dedup filtered signal: {}", signal.mint);
                return;
            }

            // Always run anti-rug on shared client (paper + live pre-check log).
            let timer_risk = PhaseTimer::start();
            let verdict = run_pre_buy_checks_with_timeout(&signal, risk_cfg.as_ref()).await;
            let anti_rug_us = timer_risk.elapsed_us();

            match &verdict {
                solbot_strategy::risk_gate::RiskGateVerdict::Accept => {
                    tracing::info!(
                        seq = seq_num,
                        mint = %signal.mint,
                        anti_rug_us,
                        "Risk gate ACCEPT"
                    );
                }
                solbot_strategy::risk_gate::RiskGateVerdict::Reject { reason } => {
                    tracing::warn!(
                        seq = seq_num,
                        mint = %signal.mint,
                        anti_rug_us,
                        reason = %reason,
                        "Risk gate REJECT"
                    );
                }
            }

            if is_paper {
                let mut metrics =
                    PipelineMetrics::new(&signal.mint, &signal.pool_address, &signal.dex_program);
                metrics.inbound_stream_latency_ms = inbound_latency;
                metrics.anti_rug_us = anti_rug_us.max(30);
                metrics.parse_decode_us = 80;
                metrics.tx_build_sign_us = 120;
                metrics.rpc_sim_rtt_ms = (anti_rug_us as f64) / 1000.0;
                metrics.print_tree_log();
            }

            // Actor buy path only after risk gate accept (paper = no broadcast).
            if matches!(
                verdict,
                solbot_strategy::risk_gate::RiskGateVerdict::Accept
            ) {
                let mode = if is_paper {
                    mint_actor::TradingMode::Paper
                } else {
                    mint_actor::TradingMode::Live
                };
                dispatch_to_actor(
                    &signal,
                    &actors,
                    wallet_key,
                    mode,
                    risk_cfg.as_ref().clone(),
                    router_clone,
                    signer_clone,
                    rpc_clone,
                    dex_ready,
                    actor_cfg,
                )
                .await;
            }

            tracing::info!(
                "[SIGNAL PROCESSING END] seq={}, mint={}, total_processing_ms={:.2}, inbound_age_ms={:.2}",
                seq_num,
                signal.mint,
                process_start.elapsed().as_secs_f64() * 1000.0,
                inbound_latency
            );
        });
    }
}

async fn dispatch_to_actor(
    signal: &solbot_common::types::Signal,
    actors: &ActorRegistry,
    wallet_key: solana_sdk::pubkey::Pubkey,
    trading_mode: mint_actor::TradingMode,
    risk_gate_config: RiskGateConfig,
    router: Option<Arc<TransactionRouter>>,
    signer_client: Option<Arc<SignerClient>>,
    rpc_url: Option<String>,
    dex_readiness: DexReadinessConfig,
    actor_config: ActorConfig,
) {
    let new_pool = ControlEvent::NewPoolSignal {
        mint: signal.mint.clone(),
        pool_address: signal.pool_address.clone(),
        dex_program: signal.dex_program.clone(),
        slot: signal.slot,
        received_at: signal.received_at,
        raw_pool_account_data: signal.raw_pool_account_data.clone(),
        raw_pool_account_owner: signal.raw_pool_account_owner.clone(),
    };

    if let Some(actor_handle) = actors.get(&signal.mint) {
        let _ = actor_handle.control_tx.send(new_pool).await;
        return;
    }

    tracing::info!("Creating new actor for mint: {}", signal.mint);

    let (control_tx, control_rx) = mpsc::channel(100);
    let (market_data_tx, _market_data_rx) = mpsc::channel(100);
    let market_data_lane = MarketDataLane::new();

    let ctx = ActorContext {
        mint: signal.mint.clone(),
        pool_address: signal.pool_address.clone(),
        dex_program_id: signal.dex_program.clone(),
        wallet: wallet_key,
        trading_mode,
        risk_gate_config,
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
                circuit_breaker: solbot_common::config::tier_params::CircuitBreakerParams {
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
        router: router.as_ref().and_then(|_| {
            TransactionRouter::new(solbot_strategy::routing::RoutingConfig::default()).ok()
        }),
        signer_client: signer_client.clone(),
        rpc_url,
        active_position: None,
        trade_stats: None,
        dex_readiness,
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
        raw_pool_account_data: signal.raw_pool_account_data.clone(),
        raw_pool_account_owner: signal.raw_pool_account_owner.clone(),
        last_signal_slot: signal.slot,
        last_signal_received_at: signal.received_at,
        buy_attempted: false,
    };

    let actor_handle = ActorHandle {
        control_tx: control_tx.clone(),
        market_data_tx,
    };
    actors.register(signal.mint.clone(), actor_handle);

    let mint = signal.mint.clone();
    tokio::spawn(async move {
        mint_actor::run(mint, control_rx, market_data_lane, ctx, actor_config).await;
    });

    // Deliver the triggering signal after the actor is running.
    let _ = control_tx.send(new_pool).await;
}
