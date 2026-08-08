//! Integration tests for MintActor trading loop.
//!
//! Tests the full end-to-end trading pipeline:
//! - Buy workflow: Signal → Risk Gate → Sizing → Tx Builder → Signer → Router
//! - Sell workflow: PriceTick → Exit Evaluation → Exit Tx → Signer → Router

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use solana_pubkey::Pubkey;
use solbot_common::config::base_config::DexReadinessConfig;
use solbot_common::config::tier_params::{
    AdaptiveParams, CapitalRange, CircuitBreakerParams, CompromiseDetection, EmergencyExitParams,
    HotWalletParams, KellyParams, PositionLimits, SizingParams, TierInfo,
};
use solbot_common::config::TierParams;
use solbot_common::types::position::{Position, PositionStatus};
use solbot_common::types::tier::CapitalTier;
use solbot_strategy::actor::mint_actor::{
    execute_buy_pipeline, execute_exit_pipeline, ActorContext, TradingMode,
};
use solbot_strategy::exit::{self, ExitAction, TpLevel};
use solbot_strategy::risk_gate::{RiskGateCache, RiskGateConfig};
use solbot_strategy::routing::{RoutingConfig, TransactionRouter};
use solbot_strategy::signer_client::SignerClient;
use solbot_strategy::state::position_lifecycle::PositionLifecycle;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Helper to start a lightweight mock JSON-RPC server that handles Risk Gate RPC calls
/// and then simulates pool account not found (returns null) on Step 3 fetch.
async fn start_mock_rpc_server_for_pool_not_found(
    mint_pubkey: String,
    pool_pubkey: String,
    dex_program_id: String,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let pool_call_count = Arc::new(AtomicUsize::new(0));

    tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let pool_count = Arc::clone(&pool_call_count);
            let mint_pk = mint_pubkey.clone();
            let pool_pk = pool_pubkey.clone();
            let dex_prog = dex_program_id.clone();

            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let req_str = String::from_utf8_lossy(&buf[..n]);

                let body = if req_str.contains("getLatestBlockhash") {
                    r#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":{"blockhash":"4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uas5tLzZ7trq76","lastValidBlockHeight":100}},"id":1}"#.to_string()
                } else if req_str.contains("simulateTransaction") {
                    r#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":{"err":null,"logs":["Program log: Instruction: Swap"],"unitsConsumed":1000}},"id":1}"#.to_string()
                } else if req_str.contains(&mint_pk) {
                    // Valid SPL mint response (82 bytes, no mint auth, no freeze auth) with Tokenkeg owner
                    let mut mint_bytes = vec![0u8; 82];
                    mint_bytes[36..44].copy_from_slice(&1_000_000_000u64.to_le_bytes());
                    mint_bytes[44] = 9;
                    mint_bytes[45] = 1;
                    let b64 = BASE64.encode(&mint_bytes);
                    format!(
                        r#"{{"jsonrpc":"2.0","result":{{"context":{{"slot":1}},"value":{{"data":["{}","base64"],"executable":false,"lamports":1000000,"owner":"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA","rentEpoch":0}}}},"id":1}}"#,
                        b64
                    )
                } else if req_str.contains(&pool_pk) {
                    let count = pool_count.fetch_add(1, Ordering::SeqCst);
                    if count < 2 {
                        // First 2 calls (during Risk Gate lp_lock_check & simulate_sell): return valid dummy pool account data
                        let mut dummy_data = vec![0u8; 400];
                        let incinerator =
                            Pubkey::from_str("1nc1nerator11111111111111111111111111111111")
                                .unwrap();
                        if dummy_data.len() >= 232 {
                            dummy_data[200..232].copy_from_slice(incinerator.as_ref());
                        }
                        let b64 = BASE64.encode(&dummy_data);
                        format!(
                            r#"{{"jsonrpc":"2.0","result":{{"context":{{"slot":1}},"value":{{"data":["{}","base64"],"executable":false,"lamports":1000000,"owner":"{}","rentEpoch":0}}}},"id":1}}"#,
                            b64, dex_prog
                        )
                    } else {
                        // 3rd call (Step 3 tx builder): return null (pool account not found)
                        r#"{"jsonrpc":"2.0","result":{"context":{"slot":1},"value":null},"id":1}"#
                            .to_string()
                    }
                } else {
                    // Any other pubkey (e.g. LP mint): return account owned by incinerator
                    let dummy_data = vec![0u8; 82];
                    let b64 = BASE64.encode(&dummy_data);
                    format!(
                        r#"{{"jsonrpc":"2.0","result":{{"context":{{"slot":1}},"value":{{"data":["{}","base64"],"executable":false,"lamports":1000000,"owner":"1nc1nerator11111111111111111111111111111111","rentEpoch":0}}}},"id":1}}"#,
                        b64
                    )
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });

    url
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cpmm_buy_pipeline_fail_closed_pool_account_not_found() {
    let mint_pubkey = Pubkey::new_unique().to_string();
    let pool_pubkey = Pubkey::new_unique().to_string();
    let dex_program_id = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C".to_string();

    let rpc_url = start_mock_rpc_server_for_pool_not_found(
        mint_pubkey.clone(),
        pool_pubkey.clone(),
        dex_program_id.clone(),
    )
    .await;

    let mut ctx = ActorContext {
        mint: mint_pubkey,
        pool_address: pool_pubkey,
        dex_program_id,
        trading_mode: TradingMode::Live,
        rpc_url: Some(rpc_url.clone()),
        wallet: Pubkey::new_unique(),
        signer_client: Some(Arc::new(SignerClient::new("/tmp/mock_signer.sock".to_string()))),
        router: Some(TransactionRouter::new(RoutingConfig::default()).unwrap()),
        tier_params: make_test_tier_params(),
        risk_gate_config: RiskGateConfig {
            rpc_url,
            rpc_client: None,
            mint_freeze_timeout_ms: 5000,
            lp_lock_timeout_ms: 10000,
            token2022_timeout_ms: 5000,
            simulate_sell_timeout_ms: 5000,
            pre_buy_checks_timeout_ms: 200,
            simulate_size_lamports: 1_000_000_000,
            token2022_allowlist: vec![],
            token2022_blocklist: vec![],
            max_signal_age_ms: 0,
            cache: RiskGateCache::new(10),
        },
        dex_readiness: DexReadinessConfig::default(),
        active_position: None,
        tier: CapitalTier::Micro,
        capital_sol: 1.0,
        trade_stats: None,
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let result = execute_buy_pipeline(&mut ctx, 0.001).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert_eq!(
        error_msg, "Failed to fetch pool account in Live mode",
        "Expected exact error 'Failed to fetch pool account in Live mode', got: {}",
        error_msg
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_dlmm_buy_pipeline_fail_closed_pool_account_not_found() {
    let mint_pubkey = Pubkey::new_unique().to_string();
    let pool_pubkey = Pubkey::new_unique().to_string();
    let dex_program_id = "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp".to_string();

    let rpc_url = start_mock_rpc_server_for_pool_not_found(
        mint_pubkey.clone(),
        pool_pubkey.clone(),
        dex_program_id.clone(),
    )
    .await;

    let mut ctx = ActorContext {
        mint: mint_pubkey,
        pool_address: pool_pubkey,
        dex_program_id,
        trading_mode: TradingMode::Live,
        rpc_url: Some(rpc_url.clone()),
        wallet: Pubkey::new_unique(),
        signer_client: Some(Arc::new(SignerClient::new("/tmp/mock_signer.sock".to_string()))),
        router: Some(TransactionRouter::new(RoutingConfig::default()).unwrap()),
        tier_params: make_test_tier_params(),
        risk_gate_config: RiskGateConfig {
            rpc_url,
            rpc_client: None,
            mint_freeze_timeout_ms: 5000,
            lp_lock_timeout_ms: 10000,
            token2022_timeout_ms: 5000,
            simulate_sell_timeout_ms: 5000,
            pre_buy_checks_timeout_ms: 200,
            simulate_size_lamports: 1_000_000_000,
            token2022_allowlist: vec![],
            token2022_blocklist: vec![],
            max_signal_age_ms: 0,
            cache: RiskGateCache::new(10),
        },
        dex_readiness: DexReadinessConfig::default(),
        active_position: None,
        tier: CapitalTier::Micro,
        capital_sol: 1.0,
        trade_stats: None,
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let result = execute_buy_pipeline(&mut ctx, 0.001).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert_eq!(
        error_msg, "Failed to fetch pool account in Live mode",
        "Expected exact error 'Failed to fetch pool account in Live mode', got: {}",
        error_msg
    );
}

#[test]
fn test_cpmm_zero_reserves_produces_zero_output_and_is_rejected() {
    use solbot_strategy::actor::mint_actor::ensure_nonzero_output;
    use solbot_strategy::tx_builder::raydium_cpmm::{CpmmFees, CpmmPoolState};

    let pool_state = CpmmPoolState {
        pool_id: Pubkey::new_unique(),
        authority: Pubkey::new_unique(),
        token_0_vault: Pubkey::new_unique(),
        token_1_vault: Pubkey::new_unique(),
        token_0_mint: Pubkey::new_unique(),
        token_1_mint: solana_pubkey::pubkey!("So11111111111111111111111111111111111111112"),
        lp_mint: Pubkey::new_unique(),
        observation_key: Pubkey::new_unique(),
        amm_config: Pubkey::new_unique(),
        fees: CpmmFees {
            trade_fee_rate: 2500,
            protocol_fee_rate: 120_000,
            fund_fee_rate: 40_000,
            creator_fee_rate: 0,
        },
        token_0_vault_balance: 0,
        token_1_vault_balance: 0,
        protocol_fees_token_0: 0,
        protocol_fees_token_1: 0,
        fund_fees_token_0: 0,
        fund_fees_token_1: 0,
        creator_fees_token_0: 0,
        creator_fees_token_1: 0,
        enable_creator_fee: false,
    }
    .with_vault_balances(0, 0);

    let expected_out = pool_state.calculate_buy_output(1_000_000_000);
    assert_eq!(expected_out, 0, "с нулевыми резервами output обязан быть 0");

    let result = ensure_nonzero_output(expected_out, "Raydium CPMM", "buy");
    assert!(result.is_err(), "нулевой output должен быть отклонён");
}

#[test]
fn test_dlmm_zero_reserves_produces_zero_output_and_is_rejected() {
    use solbot_strategy::actor::mint_actor::ensure_nonzero_output;
    use solbot_strategy::tx_builder::meteora_dlmm::DlmmPoolState;

    let pool_state = DlmmPoolState {
        lb_pair: Pubkey::new_unique(),
        bin_array_bitmap_extension: None,
        token_x_mint: Pubkey::new_unique(),
        token_y_mint: solana_pubkey::pubkey!("So11111111111111111111111111111111111111112"),
        reserve_x: Pubkey::new_unique(),
        reserve_y: Pubkey::new_unique(),
        oracle: Pubkey::new_unique(),
        active_id: 0,
        reserve_x_balance: 0,
        reserve_y_balance: 0,
        bin_step: 1,
    }
    .with_vault_balances(0, 0);

    let expected_out = pool_state.calculate_buy_output(1_000_000_000);
    assert_eq!(expected_out, 0, "с нулевыми резервами output обязан быть 0");

    let result = ensure_nonzero_output(expected_out, "Meteora DLMM", "buy");
    assert!(result.is_err(), "нулевой output должен быть отклонён");
}

#[tokio::test]
async fn test_buy_pipeline_fail_closed_no_signer_client() {
    let mut ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        trading_mode: TradingMode::Live,
        rpc_url: Some("https://api.mainnet-beta.solana.com".to_string()),
        wallet: Pubkey::new_unique(),
        signer_client: None, // Missing signer_client
        router: None,        // Will be caught by signer check first
        tier_params: make_test_tier_params(),
        risk_gate_config: make_test_risk_gate_config(),
        dex_readiness: DexReadinessConfig::default(),
        active_position: None,
        tier: CapitalTier::Micro,
        capital_sol: 1.0,
        trade_stats: None,
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let result = execute_buy_pipeline(&mut ctx, 0.001).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("No signer_client configured"),
        "Expected error about missing signer_client, got: {}",
        error_msg
    );
}

#[tokio::test]
async fn test_buy_pipeline_fail_closed_no_router() {
    let mut ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        trading_mode: TradingMode::Live,
        rpc_url: Some("https://api.mainnet-beta.solana.com".to_string()),
        wallet: Pubkey::new_unique(),
        signer_client: Some(Arc::new(SignerClient::new("/tmp/mock_signer.sock".to_string()))), // Mock signer_client present
        router: None, // Missing router - this should trigger the router guard
        tier_params: make_test_tier_params(),
        risk_gate_config: make_test_risk_gate_config(),
        dex_readiness: DexReadinessConfig::default(),
        active_position: None,
        tier: CapitalTier::Micro,
        capital_sol: 1.0,
        trade_stats: None,
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let result = execute_buy_pipeline(&mut ctx, 0.001).await;
    assert!(result.is_err());
    // Should fail on router check since signer_client is present
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("No router configured"),
        "Expected error about missing router, got: {}",
        error_msg
    );
}

#[tokio::test]
async fn test_exit_pipeline_fail_closed_no_signer_client() {
    let test_position = make_test_position();
    let mut ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        trading_mode: TradingMode::Live,
        rpc_url: Some("https://api.mainnet-beta.solana.com".to_string()),
        wallet: Pubkey::new_unique(),
        signer_client: None, // Missing signer_client
        router: None,
        tier_params: make_test_tier_params(),
        risk_gate_config: make_test_risk_gate_config(),
        dex_readiness: DexReadinessConfig::default(),
        active_position: Some(test_position.clone()),
        tier: CapitalTier::Micro,
        capital_sol: 1.0,
        trade_stats: None,
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let result =
        execute_exit_pipeline(&mut ctx, &test_position, ExitAction::HardStopLoss, 0.0005).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("No signer_client configured"),
        "Expected error about missing signer_client, got: {}",
        error_msg
    );
}

#[tokio::test]
async fn test_exit_pipeline_fail_closed_no_router() {
    let test_position = make_test_position();
    let mut ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        trading_mode: TradingMode::Live,
        rpc_url: Some("https://api.mainnet-beta.solana.com".to_string()),
        wallet: Pubkey::new_unique(),
        signer_client: Some(Arc::new(SignerClient::new("/tmp/mock_signer.sock".to_string()))), // Mock signer_client present
        router: None, // Missing router - this should trigger the router guard
        tier_params: make_test_tier_params(),
        risk_gate_config: make_test_risk_gate_config(),
        dex_readiness: DexReadinessConfig::default(),
        active_position: Some(test_position.clone()),
        tier: CapitalTier::Micro,
        capital_sol: 1.0,
        trade_stats: None,
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let result =
        execute_exit_pipeline(&mut ctx, &test_position, ExitAction::HardStopLoss, 0.0005).await;
    assert!(result.is_err());
    // Should fail on router check since signer_client is present
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("No router configured"),
        "Expected error about missing router, got: {}",
        error_msg
    );
}

fn make_test_risk_gate_config() -> RiskGateConfig {
    RiskGateConfig {
        rpc_url: "https://api.mainnet-beta.solana.com".to_string(),
        mint_freeze_timeout_ms: 5000,
        lp_lock_timeout_ms: 10000,
        token2022_timeout_ms: 5000,
        simulate_sell_timeout_ms: 5000,
        pre_buy_checks_timeout_ms: 200,
        simulate_size_lamports: 1_000_000_000,
        token2022_allowlist: vec![],
        token2022_blocklist: vec![],
        max_signal_age_ms: 0,
        cache: RiskGateCache::new(10),
    }
}

#[tokio::test]
async fn test_buy_pipeline_fail_closed_rpc_failure() {
    // Test the fetch_vault_balances helper function directly to avoid risk gate interference
    use solana_pubkey::Pubkey;
    use solbot_strategy::actor::mint_actor::fetch_vault_balances;

    let vault_a = Pubkey::new_unique();
    let vault_b = Pubkey::new_unique();

    let result = fetch_vault_balances(
        "invalid://rpc-url-that-will-fail".to_string(),
        vault_a,
        vault_b,
        "Test DEX",
    )
    .await;

    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    // Should fail on RPC fetch error
    assert!(
        error_msg.contains("Failed to fetch") || error_msg.contains("vault balance"),
        "Expected RPC fetch error, got: {}",
        error_msg
    );
}

#[tokio::test]
async fn test_ensure_nonzero_output() {
    use solbot_strategy::actor::mint_actor::ensure_nonzero_output;

    // Test zero output should bail
    let result = ensure_nonzero_output(0, "Test DEX", "buy");
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("Test DEX"));
    assert!(error_msg.contains("zero buy output"));

    // Test non-zero output should pass
    let result = ensure_nonzero_output(1000, "Test DEX", "sell");
    assert!(result.is_ok());

    // Test sell direction
    let result = ensure_nonzero_output(0, "Raydium AMM v4", "sell");
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("zero sell output"));
}

fn make_test_position() -> Position {
    Position {
        position_id: "test-pos".to_string(),
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        tier: CapitalTier::Micro,
        status: PositionStatus::Open,
        entry_price_sol: 0.001,
        size_sol: 0.1,
        size_pct_of_capital: 0.1,
        remaining_token_pct: 1.0,
        hard_sl_price_sol: 0.0005,
        trailing_active: false,
        trailing_high_water_sol: Some(0.0015),
        realized_pnl_net_sol: 0.0,
        opened_at_utc: chrono::Utc::now().timestamp() as u64,
        closed_at_utc: None,
    }
}

fn make_test_tier_params() -> TierParams {
    TierParams {
        tier: TierInfo {
            name: "Micro".to_string(),
            capital_range: CapitalRange {
                min_usd: 0,
                max_usd: 1000,
            },
        },
        sizing: SizingParams {
            base_size_pct: 0.05,
            max_size_sol: 0.5,
            slippage_hard_cap_bps: 1000,
            kelly: KellyParams {
                fraction_of_kelly: 0.25,
            },
            adaptive: AdaptiveParams {
                enabled: false,
                calibration_window_slots: 100,
                min_sample_size: 30,
                size_max_pct: 0.10,
            },
        },
        position_limits: PositionLimits {
            max_concurrent_positions: 5,
            circuit_breaker: CircuitBreakerParams {
                warning_threshold_pct: 0.10,
                daily_loss_limit_pct: 0.20,
                max_parallel_emergency_sells: 3,
            },
        },
        hot_wallet: HotWalletParams { cap_sol: 1.0 },
        compromise_detection: CompromiseDetection {
            signing_rate_threshold_5min: 100,
        },
        emergency_exit: EmergencyExitParams {
            emergency_slippage_bps: 5000,
            priority_fee_baseline: 10000,
        },
    }
}

#[test]
fn test_actor_context_creation() {
    let ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Paper,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
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
    };

    assert_eq!(ctx.trading_mode, TradingMode::Paper);
    assert_eq!(ctx.capital_sol, 10.0);
    assert!(ctx.active_position.is_none());
}

#[test]
fn test_buy_pipeline_risk_gate_rejection() {
    let mut ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Paper,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
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
    };

    // This should fail because risk gate checks will reject (no real RPC)
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(execute_buy_pipeline(&mut ctx, 1.0));

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("Risk gate") || err.to_string().contains("timeout"));
}

#[test]
fn test_sell_pipeline_take_profit() {
    let mut ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Paper,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
        router: None,
        signer_client: None,
        rpc_url: None,
        active_position: Some(Position {
            position_id: "test-pos-1".to_string(),
            mint: "TestMint111111111111111111111111111111111".to_string(),
            pool_address: "TestPool111111111111111111111111111111111".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            tier: CapitalTier::Micro,
            status: PositionStatus::Open,
            entry_price_sol: 1.0,
            size_sol: 0.5,
            size_pct_of_capital: 0.05,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000000,
            closed_at_utc: None,
        }),
        trade_stats: None,
        dex_readiness: DexReadinessConfig::default(),
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let position = ctx.active_position.clone().unwrap();
    let exit_action = ExitAction::TakeProfit {
        level: TpLevel::Tp1,
        sell_fraction_of_remaining: 0.30,
    };

    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(execute_exit_pipeline(&mut ctx, &position, exit_action, 1.5));

    assert!(result.is_ok());

    // Verify position state was updated
    if let Some(ref pos) = ctx.active_position {
        assert_eq!(pos.status, PositionStatus::Tp1Filled);
        assert!((pos.remaining_token_pct - 0.70).abs() < 0.01);
        assert!(pos.trailing_active);
    }
}

#[test]
fn test_sell_pipeline_hard_stop_loss() {
    let mut ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Paper,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
        router: None,
        signer_client: None,
        rpc_url: None,
        active_position: Some(Position {
            position_id: "test-pos-1".to_string(),
            mint: "TestMint111111111111111111111111111111111".to_string(),
            pool_address: "TestPool111111111111111111111111111111111".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            tier: CapitalTier::Micro,
            status: PositionStatus::Open,
            entry_price_sol: 1.0,
            size_sol: 0.5,
            size_pct_of_capital: 0.05,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000000,
            closed_at_utc: None,
        }),
        trade_stats: None,
        dex_readiness: DexReadinessConfig::default(),
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let position = ctx.active_position.clone().unwrap();
    let exit_action = ExitAction::HardStopLoss;

    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(execute_exit_pipeline(&mut ctx, &position, exit_action, 0.4));

    assert!(result.is_ok());

    // Verify position was closed
    if let Some(ref pos) = ctx.active_position {
        assert_eq!(pos.status, PositionStatus::Closed);
        assert_eq!(pos.remaining_token_pct, 0.0);
        assert!(pos.closed_at_utc.is_some());
    }
}

#[test]
fn test_exit_evaluation_tp1_trigger() {
    let position = Position {
        position_id: "test-pos-1".to_string(),
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        tier: CapitalTier::Micro,
        status: PositionStatus::Open,
        entry_price_sol: 1.0,
        size_sol: 0.5,
        size_pct_of_capital: 0.05,
        remaining_token_pct: 1.0,
        hard_sl_price_sol: 0.5,
        trailing_active: false,
        trailing_high_water_sol: None,
        realized_pnl_net_sol: 0.0,
        opened_at_utc: 1000000,
        closed_at_utc: None,
    };

    // Price at +60% should trigger TP1 (+50%)
    let action = exit::evaluate_exit(&position, 1.6);
    assert!(matches!(
        action,
        Some(ExitAction::TakeProfit {
            level: TpLevel::Tp1,
            ..
        })
    ));
}

#[test]
fn test_exit_evaluation_hard_sl_trigger() {
    let position = Position {
        position_id: "test-pos-1".to_string(),
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        tier: CapitalTier::Micro,
        status: PositionStatus::Open,
        entry_price_sol: 1.0,
        size_sol: 0.5,
        size_pct_of_capital: 0.05,
        remaining_token_pct: 1.0,
        hard_sl_price_sol: 0.5,
        trailing_active: false,
        trailing_high_water_sol: None,
        realized_pnl_net_sol: 0.0,
        opened_at_utc: 1000000,
        closed_at_utc: None,
    };

    // Price at -60% should trigger hard SL (-50%)
    let action = exit::evaluate_exit(&position, 0.4);
    assert!(matches!(action, Some(ExitAction::HardStopLoss)));
}

#[test]
fn test_exit_evaluation_no_trigger_in_range() {
    let position = Position {
        position_id: "test-pos-1".to_string(),
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        tier: CapitalTier::Micro,
        status: PositionStatus::Open,
        entry_price_sol: 1.0,
        size_sol: 0.5,
        size_pct_of_capital: 0.05,
        remaining_token_pct: 1.0,
        hard_sl_price_sol: 0.5,
        trailing_active: false,
        trailing_high_water_sol: None,
        realized_pnl_net_sol: 0.0,
        opened_at_utc: 1000000,
        closed_at_utc: None,
    };

    // Price at +20% should not trigger any exit
    let action = exit::evaluate_exit(&position, 1.2);
    assert!(action.is_none());
}

#[test]
fn test_actor_context_with_full_components() {
    let wallet = Pubkey::from_str("7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU").unwrap();

    let ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        wallet,
        trading_mode: TradingMode::Paper,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
        router: None,        // TODO: Add mock router
        signer_client: None, // TODO: Add mock signer client
        rpc_url: None,
        active_position: None,
        trade_stats: None,
        dex_readiness: DexReadinessConfig::default(),
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    assert_eq!(
        ctx.wallet.to_string(),
        "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU"
    );
    assert_eq!(
        ctx.dex_program_id,
        "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"
    );
    assert_eq!(ctx.trading_mode, TradingMode::Paper);
}

#[test]
fn test_trading_mode_paper_vs_live() {
    // Paper mode - should skip transaction submission
    let ctx_paper = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Paper,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
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
    };

    // Live mode - would submit transactions (mocked for test)
    let ctx_live = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Live,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
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
    };

    assert_eq!(ctx_paper.trading_mode, TradingMode::Paper);
    assert_eq!(ctx_live.trading_mode, TradingMode::Live);
    assert_ne!(ctx_paper.trading_mode, ctx_live.trading_mode);
}

#[test]
fn test_kill_switch_emergency_exit() {
    let mut ctx = ActorContext {
        mint: "TestMint111111111111111111111111111111111".to_string(),
        pool_address: "TestPool111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Paper,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
        router: None,
        signer_client: None,
        rpc_url: None,
        active_position: Some(Position {
            position_id: "test-pos-1".to_string(),
            mint: "TestMint111111111111111111111111111111111".to_string(),
            pool_address: "TestPool111111111111111111111111111111111".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            tier: CapitalTier::Micro,
            status: PositionStatus::Open,
            entry_price_sol: 1.0,
            size_sol: 0.5,
            size_pct_of_capital: 0.05,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000000,
            closed_at_utc: None,
        }),
        trade_stats: None,
        dex_readiness: DexReadinessConfig::default(),
        positions_repo: None,
        state_transitions_repo: None,
        position_lifecycle: None,
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    let position = ctx.active_position.clone().unwrap();

    // Simulate emergency exit via execute_exit_pipeline
    let exit_action = ExitAction::Emergency {
        reason: "Kill switch activated".to_string(),
    };

    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(execute_exit_pipeline(&mut ctx, &position, exit_action, 0.8));

    assert!(result.is_ok());

    // Verify position was closed
    if let Some(ref pos) = ctx.active_position {
        assert_eq!(pos.status, PositionStatus::Closed);
        assert_eq!(pos.remaining_token_pct, 0.0);
        assert!(pos.closed_at_utc.is_some());
    }
}

#[tokio::test]
async fn test_position_lifecycle_sqlite_persistence_and_transitions() {
    use solbot_strategy::state::positions_repo::PositionsRepo;
    use solbot_strategy::state::state_transitions_repo::StateTransitionsRepo;
    use std::sync::Arc;

    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    // Setup schema
    sqlx::query(
        r#"
        CREATE TABLE positions (
            position_id TEXT PRIMARY KEY,
            mint TEXT NOT NULL,
            pool_address TEXT NOT NULL,
            dex_program_id TEXT NOT NULL DEFAULT '',
            tier TEXT NOT NULL,
            status TEXT NOT NULL,
            size_sol REAL NOT NULL,
            entry_price_sol REAL NOT NULL,
            size_pct_of_capital REAL NOT NULL DEFAULT 0.0,
            remaining_token_pct REAL NOT NULL DEFAULT 1.0,
            hard_sl_price_sol REAL NOT NULL DEFAULT 0.0,
            trailing_active INTEGER NOT NULL DEFAULT 0,
            trailing_high_water_sol REAL,
            realized_pnl_net_sol REAL DEFAULT 0.0,
            opened_at_utc INTEGER NOT NULL,
            closed_at_utc INTEGER
        );
        CREATE TABLE state_transitions (
            id TEXT PRIMARY KEY,
            position_id TEXT NOT NULL,
            from_status TEXT NOT NULL,
            to_status TEXT NOT NULL,
            reason TEXT NOT NULL,
            tx_signature TEXT,
            created_at_utc INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let positions_repo = Arc::new(PositionsRepo::new(pool.clone()));
    let state_transitions_repo = Arc::new(StateTransitionsRepo::new(pool.clone()));

    let initial_pos = Position {
        position_id: "pos-lifecycle-1".to_string(),
        mint: "Mint11111111111111111111111111111111111111".to_string(),
        pool_address: "Pool11111111111111111111111111111111111111".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        tier: CapitalTier::Micro,
        status: PositionStatus::Open,
        entry_price_sol: 1.0,
        size_sol: 0.5,
        size_pct_of_capital: 0.05,
        remaining_token_pct: 1.0,
        hard_sl_price_sol: 0.5,
        trailing_active: false,
        trailing_high_water_sol: None,
        realized_pnl_net_sol: 0.0,
        opened_at_utc: 1000000,
        closed_at_utc: None,
    };

    let mut ctx = ActorContext {
        mint: initial_pos.mint.clone(),
        pool_address: initial_pos.pool_address.clone(),
        dex_program_id: initial_pos.dex_program_id.clone(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Paper,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
        router: None,
        signer_client: None,
        rpc_url: None,
        active_position: Some(initial_pos.clone()),
        trade_stats: None,
        dex_readiness: DexReadinessConfig::default(),
        positions_repo: Some(positions_repo.clone()),
        state_transitions_repo: Some(state_transitions_repo.clone()),
        position_lifecycle: Some(Arc::new(PositionLifecycle::new(pool.clone()))),
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    // 1. Insert initial position into DB
    positions_repo.insert(&initial_pos).await.unwrap();

    let open_positions = positions_repo.get_open_positions().await.unwrap();
    assert_eq!(open_positions.len(), 1);
    assert_eq!(open_positions[0].status, PositionStatus::Open);

    // 2. TP1 execution: Open -> Tp1Filled
    execute_exit_pipeline(
        &mut ctx,
        &initial_pos,
        ExitAction::TakeProfit {
            level: TpLevel::Tp1,
            sell_fraction_of_remaining: 0.3,
        },
        1.5,
    )
    .await
    .unwrap();

    let db_pos = positions_repo
        .get_position_by_id("pos-lifecycle-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(db_pos.status, PositionStatus::Tp1Filled);
    assert!((db_pos.remaining_token_pct - 0.70).abs() < 0.01);
    assert!(db_pos.trailing_active);

    // 3. TP2 execution: Tp1Filled -> Tp2Filled
    let current_pos = ctx.active_position.clone().unwrap();
    execute_exit_pipeline(
        &mut ctx,
        &current_pos,
        ExitAction::TakeProfit {
            level: TpLevel::Tp2,
            sell_fraction_of_remaining: 0.3,
        },
        2.0,
    )
    .await
    .unwrap();

    let db_pos = positions_repo
        .get_position_by_id("pos-lifecycle-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(db_pos.status, PositionStatus::Tp2Filled);
    assert!((db_pos.remaining_token_pct - 0.40).abs() < 0.01);

    // 4. TP3 execution: Tp2Filled -> Tp3Filled -> Closed (auto-transition)
    let current_pos = ctx.active_position.clone().unwrap();
    execute_exit_pipeline(
        &mut ctx,
        &current_pos,
        ExitAction::TakeProfit {
            level: TpLevel::Tp3,
            sell_fraction_of_remaining: 0.2,
        },
        3.0,
    )
    .await
    .unwrap();

    let db_pos = positions_repo
        .get_position_by_id("pos-lifecycle-1")
        .await
        .unwrap()
        .unwrap();
    // TP3 auto-transitions to Closed
    assert_eq!(db_pos.status, PositionStatus::Closed);
    assert_eq!(db_pos.remaining_token_pct, 0.0);
    assert!(db_pos.closed_at_utc.is_some());

    // Verify position is no longer returned in get_open_positions()
    let open_positions_after_close = positions_repo.get_open_positions().await.unwrap();
    assert_eq!(open_positions_after_close.len(), 0);

    // Verify all state transitions were recorded in state_transitions table
    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT from_status, to_status, reason, position_id FROM state_transitions ORDER BY created_at_utc ASC"
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].0, "open");
    assert_eq!(rows[0].1, "tp1_filled");
    assert_eq!(rows[1].0, "tp1_filled");
    assert_eq!(rows[1].1, "tp2_filled");
    assert_eq!(rows[2].0, "tp2_filled");
    assert_eq!(rows[2].1, "tp3_filled");
    assert_eq!(rows[3].0, "tp3_filled");
    assert_eq!(rows[3].1, "closed");
}

#[tokio::test]
async fn test_position_lifecycle_stuck_manual_intervention_required() {
    use solbot_common::types::MarketDataEvent;
    use solbot_strategy::actor::mint_actor::process_market_data_event;
    use solbot_strategy::state::positions_repo::PositionsRepo;
    use solbot_strategy::state::state_transitions_repo::StateTransitionsRepo;
    use std::sync::Arc;

    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    sqlx::query(
        r#"
        CREATE TABLE positions (
            position_id TEXT PRIMARY KEY,
            mint TEXT NOT NULL,
            pool_address TEXT NOT NULL,
            dex_program_id TEXT NOT NULL DEFAULT '',
            tier TEXT NOT NULL,
            status TEXT NOT NULL,
            size_sol REAL NOT NULL,
            entry_price_sol REAL NOT NULL,
            size_pct_of_capital REAL NOT NULL DEFAULT 0.0,
            remaining_token_pct REAL NOT NULL DEFAULT 1.0,
            hard_sl_price_sol REAL NOT NULL DEFAULT 0.0,
            trailing_active INTEGER NOT NULL DEFAULT 0,
            trailing_high_water_sol REAL,
            realized_pnl_net_sol REAL DEFAULT 0.0,
            opened_at_utc INTEGER NOT NULL,
            closed_at_utc INTEGER
        );
        CREATE TABLE state_transitions (
            id TEXT PRIMARY KEY,
            position_id TEXT NOT NULL,
            from_status TEXT NOT NULL,
            to_status TEXT NOT NULL,
            reason TEXT NOT NULL,
            tx_signature TEXT,
            created_at_utc INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let positions_repo = Arc::new(PositionsRepo::new(pool.clone()));
    let state_transitions_repo = Arc::new(StateTransitionsRepo::new(pool.clone()));

    let stuck_pos = Position {
        position_id: "pos-stuck-1".to_string(),
        mint: "Mint22222222222222222222222222222222222222".to_string(),
        pool_address: "Pool22222222222222222222222222222222222222".to_string(),
        dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
        tier: CapitalTier::Micro,
        status: PositionStatus::Open,
        entry_price_sol: 1.0,
        size_sol: 0.5,
        size_pct_of_capital: 0.05,
        remaining_token_pct: 1.0,
        hard_sl_price_sol: 0.5,
        trailing_active: false,
        trailing_high_water_sol: None,
        realized_pnl_net_sol: 0.0,
        opened_at_utc: 1000000,
        closed_at_utc: None,
    };

    positions_repo.insert(&stuck_pos).await.unwrap();

    let mut ctx = ActorContext {
        mint: stuck_pos.mint.clone(),
        pool_address: stuck_pos.pool_address.clone(),
        dex_program_id: stuck_pos.dex_program_id.clone(),
        wallet: Pubkey::new_unique(),
        trading_mode: TradingMode::Live, // Live mode without signer will fail
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
        router: None,
        signer_client: None, // Will trigger error in Live mode
        rpc_url: None,
        active_position: Some(stuck_pos.clone()),
        trade_stats: None,
        dex_readiness: DexReadinessConfig::default(),
        positions_repo: Some(positions_repo.clone()),
        state_transitions_repo: Some(state_transitions_repo.clone()),
        position_lifecycle: Some(Arc::new(PositionLifecycle::new(pool.clone()))),
        lifecycle_event_tx: None,
        circuit_breaker: None,
    };

    // Trigger price tick for HardStopLoss
    process_market_data_event(
        &stuck_pos.mint,
        MarketDataEvent::PriceTick {
            mint: stuck_pos.mint.clone(),
            pool_address: stuck_pos.pool_address.clone(),
            price_sol: 0.3, // -70% drop triggers hard SL
            timestamp: 1000005,
        },
        &mut ctx,
    )
    .await;

    // Verify status was changed to StuckManualInterventionRequired
    let db_pos = positions_repo
        .get_position_by_id("pos-stuck-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        db_pos.status,
        PositionStatus::StuckManualInterventionRequired
    );

    let transitions = sqlx::query_as::<_, (String, String, String)>(
        "SELECT from_status, to_status, reason FROM state_transitions WHERE position_id = 'pos-stuck-1'"
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(transitions.len(), 1);
    assert_eq!(transitions[0].0, "open");
    assert_eq!(transitions[0].1, "stuck_manual_intervention_required");
}

/// Helper function to create a test ActorContext with all required fields populated.
/// Customize by modifying returned struct after creation.
fn make_test_actor_context(
    mint: Option<String>,
    pool_address: Option<String>,
    dex_program_id: Option<String>,
    trading_mode: TradingMode,
) -> ActorContext {
    use solbot_strategy::circuit_breaker::CircuitBreaker;
    use solbot_strategy::state::position_lifecycle::PositionLifecycle;

    ActorContext {
        mint: mint.unwrap_or_else(|| "TestMint111111111111111111111111111111111".to_string()),
        pool_address: pool_address
            .unwrap_or_else(|| "TestPool111111111111111111111111111111111".to_string()),
        dex_program_id: dex_program_id
            .unwrap_or_else(|| "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string()),
        wallet: Pubkey::new_unique(),
        trading_mode,
        risk_gate_config: RiskGateConfig::default(),
        tier: CapitalTier::Micro,
        tier_params: make_test_tier_params(),
        capital_sol: 10.0,
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
    }
}
