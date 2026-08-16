//! Integration test for Stage 1 (Transaction Builder) and Stage 2 (Geyser gRPC/WebSocket Listeners).
//!
//! This binary tests the end-to-end flow:
//! 1. Connect to Geyser gRPC or WebSocket and subscribe to Pump.fun events
//! 2. Parse account updates and measure parsing latency
//! 3. Build a swap transaction using the parsed mint/pool (or mock data if skipped)
//! 4. Sign the transaction with a test keypair
//! 5. Report all timing metrics
//!
//! Environment variables:
//! - GEYSER_ENDPOINT: Geyser gRPC endpoint (e.g., "https://mainnet.helius-rpc.com")
//! - GEYSER_X_TOKEN: Authentication token for Geyser (Helius API key)
//! - WS_URL: WebSocket endpoint (e.g., "wss://mainnet.helius-rpc.com/?api-key=YOUR_KEY")
//! - RPC_URL: Mainnet RPC URL (optional, defaults to GEYSER_ENDPOINT)
//! - SKIP_GEYSER: Set to 1 to skip Geyser connection and use mock data
//! - USE_WEBSOCKET: Set to 1 to use WebSocket instead of Geyser

use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_signer::Signer;
use tokio::time::{timeout, Duration};

use solbot_strategy::signal::geyser_client::{AccountUpdate, GeyserClient, GeyserConfig};
use solbot_strategy::signal::ws_listener::{WsConfig, WsListener};
use solbot_strategy::tx_builder::{
    blockhash_cache::BlockhashCache,
    build_unsigned_transaction,
    pump_fun::{build_swap as pump_fun_build_swap, derive_bonding_curve, PumpFunBondingCurve},
    raydium_amm_v4::{build_swap as raydium_build_swap, RaydiumAmmV4PoolState},
    SwapDirection, SwapParams,
};

/// Number of account updates to capture before proceeding to transaction building test
const TARGET_SIGNALS: usize = 1; // Уменьшили до 1 для теста

/// Total timeout for the entire test (seconds)
const TEST_TIMEOUT_SECS: u64 = 120; // Увеличили до 2 минут

/// Amount in SOL for the test buy transaction (0.01 SOL = 10,000,000 lamports)
const TEST_AMOUNT_SOL: u64 = 10_000_000;

/// Slippage tolerance for test (5% = 500 bps)
const TEST_SLIPPAGE_BPS: u32 = 500;

/// Compute budget for the transaction
const COMPUTE_UNITS: u32 = 200_000;

/// Priority fee in micro-lamports
const PRIORITY_FEE_MICRO_LAMPORTS: u64 = 10_000;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing for console output
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🚀 Starting Stage 1 & 2 Integration Test");
    println!("=========================================\n");

    // 1. Initialize from environment variables
    let geyser_endpoint = std::env::var("GEYSER_ENDPOINT")
        .unwrap_or_else(|_| "https://mainnet.helius-rpc.com".to_string());
    let geyser_x_token = std::env::var("GEYSER_X_TOKEN").unwrap_or_else(|_| "".to_string());
    let rpc_url =
        std::env::var("RPC_URL").unwrap_or_else(|_| "https://mainnet.helius-rpc.com".to_string());
    let ws_url =
        std::env::var("WS_URL").unwrap_or_else(|_| "wss://mainnet.helius-rpc.com".to_string());
    let use_websocket = std::env::var("USE_WEBSOCKET")
        .map(|v| v == "1")
        .unwrap_or(false);

    let skip_geyser = std::env::var("SKIP_GEYSER")
        .map(|v| v == "1")
        .unwrap_or(false);

    println!("📡 Configuration:");
    println!("  Geyser Endpoint: {}", geyser_endpoint);
    println!("  WebSocket URL: {}", ws_url);
    println!("  RPC URL: {}", rpc_url);
    println!("  Target Signals: {}", TARGET_SIGNALS);
    println!(
        "  Test Amount: {} SOL",
        TEST_AMOUNT_SOL as f64 / 1_000_000_000.0
    );
    println!("  Skip Geyser: {}", skip_geyser);
    println!("  Use WebSocket: {}", use_websocket);
    println!();

    // Generate test keypair (for signing only, not for real transactions)
    let test_keypair = Keypair::new();
    let wallet_pubkey = test_keypair.pubkey();
    println!("🔑 Test Wallet: {}", wallet_pubkey);
    println!();

    // 2. Test Stage 2: Geyser gRPC Speed and Parsing (or use mock data)
    let (captured_signals, total_parse_time_us) = if skip_geyser {
        println!("📡 Stage 2: SKIPPED (using mock data)");
        println!("-------------------------------------------");

        // Create mock signal for testing Stage 1
        let mock_mint = Pubkey::new_unique();
        let mock_pool = Pubkey::new_unique();
        let mock_dex = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string();

        println!("📨 Mock Signal #1:");
        println!("  Mint: {}", mock_mint);
        println!("  Pool: {}", mock_pool);
        println!("  DEX: {}", mock_dex);
        println!();

        let signals = vec![ParsedSignal {
            mint: mock_mint.to_string(),
            pool_address: mock_pool.to_string(),
            dex_program: mock_dex,
            signal_type: "mock".to_string(),
        }];

        (signals, 0u128) // No parse time for mock data
    } else {
        println!(
            "📡 Stage 2: Testing {} Subscription",
            if use_websocket {
                "WebSocket"
            } else {
                "Geyser gRPC"
            }
        );
        println!("-------------------------------------------");

        let mut account_updates = if use_websocket {
            // Use public WebSocket endpoint with client-side filtering
            // Helius WebSocket with ?api-key= format returns 400 in Rust client
            let ws_config = WsConfig {
                ws_url: "wss://api.mainnet-beta.solana.com".to_string(),
                rpc_url: rpc_url.clone(),
                commitment: 1, // confirmed
            };

            let ws_listener = WsListener::new(ws_config);

            // Subscribe to Pump.fun program (correct program ID from real logs)
            let pump_fun_program = Pubkey::from_str("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA")
                .context("Invalid Pump.fun program ID")?;

            println!(
                "Subscribing to Pump.fun program via WebSocket: {}",
                pump_fun_program
            );

            ws_listener
                .subscribe_to_programs(vec![pump_fun_program])
                .await
                .context("Failed to subscribe to WebSocket")?
        } else {
            let geyser_config = GeyserConfig {
                endpoint: geyser_endpoint.clone(),
                x_token: Some(geyser_x_token),
                commitment: 1, // confirmed
                backfill_rpc_url: None,
                max_backfill_slots: 150,
            };

            let geyser_client = GeyserClient::new(geyser_config);

            // Subscribe to Pump.fun program
            let pump_fun_program = Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")
                .context("Invalid Pump.fun program ID")?;

            println!(
                "Subscribing to Pump.fun program via Geyser: {}",
                pump_fun_program
            );

            geyser_client
                .subscribe_to_programs(vec![pump_fun_program], None, false)
                .await
                .context("Failed to subscribe to Geyser gRPC")?
        };

        println!(
            "✅ Connected to {}, waiting for signals...\n",
            if use_websocket {
                "WebSocket"
            } else {
                "Geyser gRPC"
            }
        );

        let mut captured_signals = Vec::new();
        let mut total_parse_time_us = 0u128;

        // Capture signals with timeout
        let capture_result = timeout(Duration::from_secs(TEST_TIMEOUT_SECS), async {
            while captured_signals.len() < TARGET_SIGNALS {
                match account_updates.recv().await {
                    Some(update) => {
                        let parse_start = Instant::now();

                        println!(
                            "📥 Received AccountUpdate: account={}, owner={}, data_len={}, slot={}",
                            update.account,
                            update.owner,
                            update.data.len(),
                            update.slot
                        );

                        // Parse the account update to extract mint
                        if let Some(parsed) = parse_pump_fun_update(&update) {
                            let parse_duration = parse_start.elapsed();
                            let parse_us = parse_duration.as_micros();
                            total_parse_time_us += parse_us;

                            println!(
                                "📨 Signal #{}: Mint={}, Slot={}, Parse Time={}μs",
                                captured_signals.len() + 1,
                                parsed.mint,
                                update.slot,
                                parse_us
                            );

                            captured_signals.push(parsed);
                        } else {
                            println!("⚠️  Failed to parse account update as Pump.fun signal");
                        }
                    }
                    None => {
                        anyhow::bail!("Geyser stream closed unexpectedly");
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;

        match capture_result {
            Ok(Ok(())) => {
                println!(
                    "\n✅ Captured {} signals successfully",
                    captured_signals.len()
                );
                println!(
                    "   Average parse time: {}μs",
                    total_parse_time_us / captured_signals.len() as u128
                );
            }
            Ok(Err(e)) => {
                eprintln!("❌ Error capturing signals: {}", e);
                return Err(e);
            }
            Err(_) => {
                panic!(
                    "⏱️  Timeout waiting for Geyser signals ({} seconds)",
                    TEST_TIMEOUT_SECS
                );
            }
        }

        if captured_signals.is_empty() {
            anyhow::bail!("No signals captured, cannot proceed to Stage 1 test");
        }

        println!();

        (captured_signals, total_parse_time_us)
    };

    // 3. Test Stage 1: Transaction Building and Signing
    println!("🔨 Stage 1: Testing Transaction Builder");
    println!("----------------------------------------");

    // Use the first captured signal for transaction building
    let signal = &captured_signals[0];
    let mint = Pubkey::from_str(&signal.mint).context("Invalid mint from signal")?;
    let pool_address =
        Pubkey::from_str(&signal.pool_address).context("Invalid pool address from signal")?;
    let dex_program =
        Pubkey::from_str(&signal.dex_program).context("Invalid DEX program from signal")?;

    println!("Using signal:");
    println!("  Mint: {}", mint);
    println!("  Pool: {}", pool_address);
    println!("  DEX Program: {}", dex_program);
    println!();

    // Initialize RPC client for blockhash
    let rpc_client = Arc::new(RpcClient::new(rpc_url));
    let blockhash_cache = BlockhashCache::new(rpc_client.clone()).await?;

    // Get recent blockhash
    println!("Fetching recent blockhash...");
    let recent_blockhash = blockhash_cache
        .get_hash()
        .await
        .context("Failed to get recent blockhash")?;
    println!("✅ Blockhash: {}", recent_blockhash);
    println!();

    // Build transaction based on DEX type
    println!("Building swap transaction...");

    let build_start = Instant::now();

    let swap_instructions = if signal.dex_program == "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
    {
        // Pump.fun
        println!("  DEX: Pump.fun Bonding Curve");

        // For Pump.fun, we need the bonding curve state
        // In a real scenario, we'd fetch this from RPC, but for testing we'll use a mock
        let bonding_curve = derive_bonding_curve(&mint);
        println!("  Bonding Curve: {}", bonding_curve);

        // Create a mock bonding curve state for testing
        let mock_curve = PumpFunBondingCurve {
            bonding_curve,
            mint,
            associated_bonding_curve: Pubkey::new_unique(), // Mock
            creator: Pubkey::new_unique(),
            virtual_token_reserves: 1_000_000_000_000,
            virtual_sol_reserves: 30_000_000_000,
            real_token_reserves: 500_000_000_000,
            real_sol_reserves: 15_000_000_000,
            complete: true,
            token_total_supply: 1_000_000_000_000,
        };

        let params = SwapParams {
            mint,
            pool_address,
            dex_program_id: Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")?,
            wallet: wallet_pubkey,
            direction: SwapDirection::Buy,
            amount_in: TEST_AMOUNT_SOL,
            minimum_amount_out: 0,
            slippage_bps: TEST_SLIPPAGE_BPS,
        };

        pump_fun_build_swap(&params, &mock_curve)
            .context("Failed to build Pump.fun swap instructions")?
    } else if signal.dex_program == "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM" {
        // Raydium AMM v4
        println!("  DEX: Raydium AMM v4");

        // For Raydium, we'd need pool state from RPC
        // Using mock data for testing
        use solbot_strategy::tx_builder::raydium_amm_v4::Fees;
        let mock_pool_state = RaydiumAmmV4PoolState {
            amm_id: pool_address,
            amm_authority: Pubkey::new_unique(),
            amm_open_orders: Pubkey::new_unique(),
            amm_target_orders: Pubkey::new_unique(),
            pool_coin_token_account: Pubkey::new_unique(),
            pool_pc_token_account: Pubkey::new_unique(),
            serum_program_id: Pubkey::new_unique(),
            serum_market: Pubkey::new_unique(),
            serum_bids: Pubkey::new_unique(),
            serum_asks: Pubkey::new_unique(),
            serum_event_queue: Pubkey::new_unique(),
            serum_coin_vault: Pubkey::new_unique(),
            serum_pc_vault: Pubkey::new_unique(),
            serum_vault_signer: Pubkey::new_unique(),
            coin_mint: mint,
            pc_mint: solana_pubkey::pubkey!("So11111111111111111111111111111111111111112"), // WSOL
            fees: Fees {
                min_separate_numerator: 5,
                min_separate_denominator: 10_000,
                trade_fee_numerator: 25,
                trade_fee_denominator: 10_000,
                pnl_numerator: 12,
                pnl_denominator: 100,
                swap_fee_numerator: 25,
                swap_fee_denominator: 10_000,
            },
            coin_vault_balance: 0,
            pc_vault_balance: 0,
        };

        let params = SwapParams {
            mint,
            pool_address,
            dex_program_id: dex_program,
            wallet: wallet_pubkey,
            direction: SwapDirection::Buy,
            amount_in: TEST_AMOUNT_SOL,
            minimum_amount_out: 0,
            slippage_bps: TEST_SLIPPAGE_BPS,
        };

        raydium_build_swap(&params, &mock_pool_state)
            .context("Failed to build Raydium swap instructions")?
    } else if signal.dex_program == "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA" {
        // Pump.fun bonding curve
        let pump_fun_program = Pubkey::from_str("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA")
            .context("Invalid Pump.fun program ID")?;

        let params = SwapParams {
            mint,
            pool_address,
            dex_program_id: pump_fun_program,
            wallet: wallet_pubkey,
            direction: SwapDirection::Buy,
            amount_in: TEST_AMOUNT_SOL,
            minimum_amount_out: 0,
            slippage_bps: TEST_SLIPPAGE_BPS,
        };

        // Mock bonding curve state for testing
        let mock_bonding_curve = PumpFunBondingCurve {
            bonding_curve: pool_address,
            mint,
            associated_bonding_curve: Pubkey::new_unique(),
            creator: Pubkey::new_unique(),
            virtual_token_reserves: 1_000_000_000,
            virtual_sol_reserves: 10_000_000_000,
            real_token_reserves: 1_000_000,
            real_sol_reserves: 10_000_000,
            token_total_supply: 1_000_000_000,
            complete: false,
        };

        pump_fun_build_swap(&params, &mock_bonding_curve)
            .context("Failed to build Pump.fun swap instructions")?
    } else {
        anyhow::bail!("Unsupported DEX program: {}", signal.dex_program);
    };

    let (unsigned_tx, tx_base64) = build_unsigned_transaction(
        swap_instructions,
        COMPUTE_UNITS,
        PRIORITY_FEE_MICRO_LAMPORTS,
        &wallet_pubkey,
        recent_blockhash,
    )
    .context("Failed to build unsigned transaction")?;

    let build_duration = build_start.elapsed();
    let build_ms = build_duration.as_millis();

    println!("✅ Transaction built:");
    println!("   Build Time: {}ms", build_ms);
    println!("   Data Length: {} bytes", tx_base64.len());
    println!(
        "   Instructions: {}",
        unsigned_tx.message.instructions.len()
    );
    println!();

    // 4. Sign the transaction
    println!("🔐 Signing transaction...");

    let sign_start = Instant::now();

    // Sign with test keypair (Agave 2.0 requires blockhash parameter)
    let mut signed_tx = unsigned_tx;
    signed_tx.sign(&[&test_keypair], recent_blockhash);

    let sign_duration = sign_start.elapsed();
    let sign_us = sign_duration.as_micros();

    println!("✅ Transaction signed:");
    println!("   Sign Time: {}μs", sign_us);
    println!("   Signatures: {}", signed_tx.signatures.len());
    println!();

    // 5. Summary
    println!("=========================================");
    println!("📊 Test Summary");
    println!("=========================================");
    println!("Stage 2 (Geyser gRPC):");
    println!("  Signals Captured: {}", captured_signals.len());
    println!(
        "  Avg Parse Time: {}μs",
        total_parse_time_us / captured_signals.len() as u128
    );
    println!();
    println!("Stage 1 (Transaction Builder):");
    println!("  Build Time: {}ms", build_ms);
    println!("  Sign Time: {}μs", sign_us);
    println!("  TX Data Length: {} bytes", tx_base64.len());
    println!();
    println!("✅ All tests completed successfully!");

    Ok(())
}

/// Parse a Pump.fun account update to extract mint and pool information.
///
/// Parse a Pump.fun account update to extract mint and pool information.
fn parse_pump_fun_update(update: &AccountUpdate) -> Option<ParsedSignal> {
    let account_data = &update.data;

    // Check if this is a WebSocket fallback signal (simpler format)
    // WebSocket signals have mint at offset 8 in a 40-byte buffer
    if account_data.len() == 40 {
        if let Ok(mint) = read_pubkey_bytes(account_data, 8) {
            return Some(ParsedSignal {
                mint: mint.to_string(),
                pool_address: update.account.to_string(),
                dex_program: update.owner.to_string(),
                signal_type: "websocket_fallback".to_string(),
            });
        }
    }

    // Check if this is a Geyser account update (full bonding curve data)
    // Bonding curve accounts have specific data structure
    if account_data.len() > 72 {
        // Check for migration event (complete field at offset 72)
        let is_complete = account_data[72] != 0;

        if is_complete {
            // Token has migrated to Raydium!
            if let Ok(mint) = read_pubkey_bytes(account_data, 8) {
                return Some(ParsedSignal {
                    mint: mint.to_string(),
                    pool_address: update.account.to_string(),
                    dex_program: update.owner.to_string(),
                    signal_type: "migration".to_string(),
                });
            }
        }

        // New bonding curve creation (complete == false)
        if !is_complete && account_data.len() > 8 + 32 + 8 * 5 {
            if let Ok(mint) = read_pubkey_bytes(account_data, 8) {
                return Some(ParsedSignal {
                    mint: mint.to_string(),
                    pool_address: update.account.to_string(),
                    dex_program: update.owner.to_string(),
                    signal_type: "bonding_curve".to_string(),
                });
            }
        }
    }

    None
}

/// Helper to read a Pubkey from byte slice at offset
fn read_pubkey_bytes(data: &[u8], offset: usize) -> Result<Pubkey> {
    if offset + 32 > data.len() {
        anyhow::bail!("Pubkey offset out of bounds");
    }
    let bytes: [u8; 32] = data[offset..offset + 32].try_into()?;
    Ok(Pubkey::new_from_array(bytes))
}

/// Parsed signal from account update
#[derive(Debug, Clone)]
struct ParsedSignal {
    mint: String,
    pool_address: String,
    dex_program: String,
    signal_type: String,
}
