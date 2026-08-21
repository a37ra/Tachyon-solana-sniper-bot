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
use solbot_strategy::smart_money::WatchlistCache;
use solbot_strategy::startup::{self, hydration::HydrationOptions};
use solbot_strategy::state::positions_repo::PositionsRepo;
use solbot_strategy::state::smart_money_hits_repo::SmartMoneyHitsRepo;
use solbot_strategy::state::wallet_watchlist_repo::WalletWatchlistRepo;
use solbot_strategy::observability;
use sqlx::SqlitePool;
use std::env;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    if let Ok(endpoint) = env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        if !endpoint.trim().is_empty() {
            if let Err(err) = solbot_common::otel::init::init_telemetry("solbot-strategy", &endpoint) {
                tracing::warn!(endpoint, error = %err, "OpenTelemetry init failed; falling back to stdout logging");
            }
        } else {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_target(false)
                .with_ansi(false)
                .try_init();
        }
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_target(false)
            .with_ansi(false)
            .try_init();
    }

    // Default: logsSubscribe create-ix only (not programSubscribe account updates).
    // Account-update path falsely treats first-seen old pools as "new".
    // Opt-in legacy: SIGNAL_SOURCE=helius_ws|both|helius_both
    if env::var("SIGNAL_SOURCE").is_err() {
        env::set_var("SIGNAL_SOURCE", "helius_logs");
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
        println!(
            "   Paper trade journal: {}",
            solbot_strategy::paper_trade_log::path_hint()
        );
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

    // Spawn Prometheus /metrics HTTP endpoint (port 9090)
    let metrics_port: u16 = env::var("METRICS_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(9090);
    observability::spawn_metrics_server(metrics_port);

    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:solbot.db".to_string());
    let sqlite_pool = match SqlitePool::connect(&db_url).await {
        Ok(pool) => Some(pool),
        Err(e) => {
            tracing::warn!("SQLite DB connection error ( ephemerals/dev ): {}", e);
            None
        }
    };

    let positions_repo = sqlite_pool.as_ref().map(|p| Arc::new(PositionsRepo::new(p.clone())));
    let smart_money_hits_repo = sqlite_pool.as_ref().map(|p| Arc::new(SmartMoneyHitsRepo::new(p.clone())));
    let wallet_watchlist_repo = sqlite_pool.as_ref().map(|p| Arc::new(WalletWatchlistRepo::new(p.clone())));

    let watchlist_cache = Arc::new(WatchlistCache::new());
    if let Some(ref repo) = wallet_watchlist_repo {
        if let Ok(n) = watchlist_cache.reload_from_repo(repo).await {
            tracing::info!(count = n, "Smart money watchlist cache hydrated from DB");
        }
    }

    let actor_config = ActorConfig::default();
    let actor_registry = ActorRegistry::new();

    let rpc_url = resolve_rpc_url();
    let shared_rpc = Arc::new(
        solana_rpc_client::nonblocking::rpc_client::RpcClient::new_with_commitment(
            rpc_url.clone(),
            solana_sdk::commitment_config::CommitmentConfig::processed(),
        ),
    );
    let blocking_rpc = Arc::new(solana_rpc_client::rpc_client::RpcClient::new(rpc_url.clone()));

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

    let backfill_provider = sqlite_pool
        .as_ref()
        .map(|p| solbot_strategy::signal::geyser_client::ExtendedHistoryProvider::new(blocking_rpc.clone(), Arc::new(p.clone())));

    // Stage 7: Full System Hydration (TZ §2.4)
    let hydration_opts = HydrationOptions {
        is_live: !is_paper,
        signer_client: signer_client.as_deref(),
        positions_repo: positions_repo.as_deref(),
        rpc: Some(&blocking_rpc),
        async_rpc: None,
        wallet_pubkey: Some(wallet_pubkey),
        backfill_provider: backfill_provider.as_ref(),
    };
    let hydration_report = startup::hydration::hydrate_system_state_with_opts(&hydration_opts).await?;

    // Pump fee_recipient from on-chain Global — no hardcoded tip/fee addresses.
    solbot_strategy::tx_builder::pump_fun::load_fee_recipient_from_global(shared_rpc.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("pump Global fee_recipient load failed (refusing start): {e}"))?;

    let dedup_filter = DedupFilter::new(30);
    let signal_source = env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "helius_logs".into());

    // logsSubscribe create-ix is the primary source. programSubscribe is opt-in only
    // (helius_ws / both) — it cannot distinguish new pools from first-seen old ones.
    let use_account_ws = matches!(
        signal_source.as_str(),
        "helius_ws" | "geyser_grpc" | "both" | "helius_both"
    );
    let use_create_ix = matches!(
        signal_source.as_str(),
        "helius_logs" | "create_ix" | "both" | "helius_both"
    ) || env::var("SOLBOT_CREATE_IX")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || !use_account_ws;

    let mut signal_rxs: Vec<(mpsc::Receiver<solbot_common::types::Signal>, &'static str)> =
        Vec::new();

    if use_account_ws {
        let raydium_listener = RaydiumListener::new();
        let meteora_listener = MeteoraListener::new();
        let pump_fun_listener = PumpFunListener::new();
        signal_rxs.push((
            raydium_listener.subscribe_to_pool_creation().await?,
            "raydium_ws",
        ));
        signal_rxs.push((
            meteora_listener.subscribe_to_pool_creation().await?,
            "meteora_ws",
        ));
        signal_rxs.push((
            pump_fun_listener.subscribe_to_bonding_curve().await?,
            "pump_fun_ws",
        ));
    }

    if use_create_ix {
        use solana_pubkey::Pubkey;
        use std::str::FromStr;
        let programs = vec![
            Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8")?,
            Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C")?,
            Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK")?,
            Pubkey::from_str("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo")?,
            Pubkey::from_str("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG")?,
            Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")?,
        ];
        let create_rx = solbot_strategy::signal::create_ix_source::subscribe_create_ix_signals(
            programs,
            shared_rpc.clone(),
        )
        .await?;
        signal_rxs.push((create_rx, "create_ix_logs"));
        tracing::info!("create-ix logsSubscribe source enabled");
    }

    if signal_rxs.is_empty() {
        anyhow::bail!(
            "No signal sources: set SIGNAL_SOURCE=helius_ws|helius_logs|both (got '{signal_source}')"
        );
    }

    for (rx, label) in signal_rxs {
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
        let pos_repo = positions_repo.clone();
        let sm_hits_repo = smart_money_hits_repo.clone();
        let wl_cache = watchlist_cache.clone();
        tokio::spawn(async move {
            tracing::info!(listener = label, "Signal processor started");
            process_signals(
                rx, dedup, actors, paper, wallet, router_c, signer_c, rpc_c, dex_r, actor_cfg, risk,
                pos_repo, sm_hits_repo, Some(wl_cache),
            )
            .await;
        });
    }

    solbot_strategy::signal::pipeline_stats::spawn_stats_logger();
    tracing::info!(
        public_rpc = %std::env::var("SOLBOT_PUBLIC_RPC").unwrap_or_default(),
        max_signal_age_ms = %std::env::var("SOLBOT_MAX_SIGNAL_AGE_MS").unwrap_or_else(|_| "2000".into()),
        rpc_url_host = %rpc_url.split('?').next().unwrap_or(&rpc_url),
        "Actor system ready (pipeline stats are GLOBAL, not per-listener)"
    );

    tokio::signal::ctrl_c().await?;
    tracing::info!("Received shutdown signal");
    let final_stats = solbot_strategy::signal::pipeline_stats::snapshot();
    tracing::info!(?final_stats, "PIPELINE STATS final");

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
    positions_repo: Option<Arc<PositionsRepo>>,
    smart_money_hits_repo: Option<Arc<SmartMoneyHitsRepo>>,
    watchlist_cache: Option<Arc<WatchlistCache>>,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    use solbot_strategy::signal::pipeline_stats;
    let signal_count = StdArc::new(AtomicUsize::new(0));
    // Throughput: permits / mean_check_latency. After noise filter, default 5 is enough
    // for paper; live private RPC can raise via SOLBOT_MAX_CONCURRENT_SIGNALS.
    let max_concurrent = env::var("SOLBOT_MAX_CONCURRENT_SIGNALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let semaphore = StdArc::new(tokio::sync::Semaphore::new(max_concurrent));
    // Bound memory: mpsc from listeners is 256; if queue is full, senders await (backpressure).
    // Additional hard drop when receiver-side backlog exceeds this while waiting for permit.
    let max_queue_depth: usize = env::var("SOLBOT_MAX_QUEUE_DEPTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);

    // Paper public profile default 2000ms; override via SOLBOT_MAX_SIGNAL_AGE_MS.
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

        if queue_depth > max_queue_depth {
            let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
            pipeline_stats::inc_drop("queue_overflow");
            tracing::warn!(
                "[SIGNAL DROPPED - QUEUE] seq={}, mint={}, queue_depth={} > max={}",
                seq_num,
                signal.mint,
                queue_depth,
                max_queue_depth
            );
            continue;
        }

        if inbound_latency > max_signal_age_ms {
            let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
            let age_u = inbound_latency as u64;
            pipeline_stats::inc_drop("stale");
            pipeline_stats::record_stale_age_ms(age_u);
            tracing::warn!(
                "[SIGNAL DROPPED - STALE] seq={}, mint={}, inbound_age_ms={:.2} > {}ms threshold",
                seq_num,
                signal.mint,
                inbound_latency,
                max_signal_age_ms
            );
            continue;
        }

        if !solbot_strategy::signal::pool_signal::is_plausible_spl_mint(
            &solana_sdk::pubkey::Pubkey::try_from(signal.mint.as_str()).unwrap_or_default(),
        ) {
            let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
            pipeline_stats::inc_drop("bad_mint");
            tracing::warn!(
                "[SIGNAL DROPPED - BAD_MINT] seq={}, mint={}",
                seq_num,
                signal.mint
            );
            continue;
        }

        let seq_num = signal_count.fetch_add(1, Ordering::SeqCst);
        pipeline_stats::inc_queued();
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
        let positions_repo_clone = positions_repo.clone();
        let smart_money_hits_repo_clone = smart_money_hits_repo.clone();
        let watchlist_cache_clone = watchlist_cache.clone();

        let permit = match StdArc::clone(&sem).acquire_owned().await {
            Ok(p) => p,
            Err(_) => continue,
        };

        tokio::spawn(async move {
            let _permit = permit;
            let process_start = Instant::now();
            let signal_span = tracing::info_span!("signal_received", mint = %signal.mint, pool = %signal.pool_address, dex = %signal.dex_program);
            let _enter = signal_span.enter();
            drop(_enter); // Don't hold across await — just record the span event

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
                    pipeline_stats::inc_accept();
                    solbot_strategy::paper_trade_log::log_accept(
                        &signal.mint,
                        &signal.pool_address,
                        &signal.dex_program,
                        anti_rug_us as f64 / 1000.0,
                    );
                    tracing::info!(
                        seq = seq_num,
                        mint = %signal.mint,
                        anti_rug_us,
                        "Risk gate ACCEPT"
                    );
                }
                solbot_strategy::risk_gate::RiskGateVerdict::Reject { reason } => {
                    let bucket = reject_reason_bucket(reason);
                    pipeline_stats::inc_reject(&bucket);
                    if is_paper {
                        solbot_strategy::paper_trade_log::log_reject(&signal.mint, reason);
                    }
                    tracing::warn!(
                        seq = seq_num,
                        mint = %signal.mint,
                        anti_rug_us,
                        reason = %reason,
                        bucket = %bucket,
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
                    positions_repo_clone,
                    smart_money_hits_repo_clone,
                    watchlist_cache_clone,
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

fn reject_reason_bucket(reason: &str) -> String {
    let r = reason.to_lowercase();
    if r.contains("stale signal") {
        "stale".into()
    } else if r.contains("timeout") || r.contains("timed out") {
        "rpc_timeout".into()
    } else if r.contains("mint account not available") || r.contains("not available in context") {
        "mint_unavailable".into()
    } else if r.contains("failed to parse mint") || r.contains("invalidstate") {
        "mint_parse".into()
    } else if r.contains("not spl token") {
        "bad_mint_owner".into()
    } else if r.contains("freeze") || r.contains("mint_authority") || r.contains("mint authority") {
        "freeze_or_mint_auth".into()
    } else if r.contains("lp_lock") || r.contains("lp lock") || r.contains("unsupported dex program for lp") {
        "lp_lock".into()
    } else if r.contains("token2022") || r.contains("extension") {
        "token2022".into()
    } else if r.contains("unsupported dex program for simulation") {
        "simulate_unsupported_dex".into()
    } else if r.contains("builder_error") || r.contains("builder error") {
        "builder_error".into()
    } else if r.contains("honeypot") || r.contains("simulate") || r.contains("tax") {
        "simulate_sell".into()
    } else if r.contains("failed to fetch pre-buy") {
        "context_fetch".into()
    } else {
        // Keep raw prefix so "other" never swallows unknown reasons silently in greps.
        format!("other:{}", reason.chars().take(48).collect::<String>())
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
    positions_repo: Option<Arc<PositionsRepo>>,
    smart_money_hits_repo: Option<Arc<SmartMoneyHitsRepo>>,
    watchlist_cache: Option<Arc<WatchlistCache>>,
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
        positions_repo: positions_repo.clone(),
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
        raw_pool_account_data: signal.raw_pool_account_data.clone(),
        raw_pool_account_owner: signal.raw_pool_account_owner.clone(),
        last_signal_slot: signal.slot,
        last_signal_received_at: signal.received_at,
        buy_attempted: false,
        fee_ledger: solbot_strategy::pnl::PositionFeeLedger::new(),
        smart_money_mode: solbot_strategy::smart_money::SmartMoneyMode::from_env(),
        watchlist_cache: watchlist_cache.as_ref().map(|c| (**c).clone()),
        smart_money_hits_repo: smart_money_hits_repo.clone(),
        wsol_manager: solbot_strategy::wsol::WsolManager::default(),
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
