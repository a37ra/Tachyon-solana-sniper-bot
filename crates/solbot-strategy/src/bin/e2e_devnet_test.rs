//! End-to-end integration test on Solana devnet.
//!
//! This binary tests the full trading pipeline:
//! - Synthetic signal (using existing devnet pool)
//! - Risk gate checks (real RPC calls to devnet)
//! - Position sizing
//! - Transaction building
//! - Signer client (gRPC over UDS)
//! - Routing submission (real sendTransaction to devnet)
//! - Confirmation polling
//!
//! SAFETY: Uses separate test keypair and test socket path.
//! Only works with devnet RPC endpoints.

use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::signature::Signer;
use solana_transaction::Transaction;
use std::time::{Duration, Instant};
use tracing::{error, warn};
use tracing_subscriber;

use solbot_common::types::intent::{IntentType, SignerIntentRequest, VerdictStatus};
use solbot_strategy::risk_gate::lp_lock_check;
use solbot_strategy::risk_gate::mint_freeze_authority;
use solbot_strategy::risk_gate::token2022_extensions;
use solbot_strategy::signer_client::SignerClient;

/// Devnet RPC endpoint
const DEVNET_RPC: &str = "https://api.devnet.solana.com";

/// Known devnet Raydium AMM v4 pool (SOL/USDC)
/// This is a real pool on devnet for testing
const DEVNET_RAYDIUM_POOL: &str = "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUdBkw3feFWFN7";

/// Raydium AMM v4 program ID
const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

/// Test signer socket path (separate from production)
const TEST_SIGNER_SOCKET: &str = "/tmp/solbot-signer-test.sock";

/// Test keypair path
const TEST_KEYPAIR_PATH: &str = "/tmp/test-keypair.json";

#[derive(Debug)]
struct LatencyMetrics {
    signal_received: u64,
    rpc_ping: u64,
    risk_gate_total: u64,
    risk_gate_mint_freeze: u64,
    sizing_compute: u64,
    build_tx: u64,
    fetch_blockhash: u64,
    signer_roundtrip: u64,
    routing_submit: u64,
    confirmation: u64,
    end_to_end: u64,
}

impl LatencyMetrics {
    fn new() -> Self {
        Self {
            signal_received: 0,
            rpc_ping: 0,
            risk_gate_total: 0,
            risk_gate_mint_freeze: 0,
            sizing_compute: 0,
            build_tx: 0,
            fetch_blockhash: 0,
            signer_roundtrip: 0,
            routing_submit: 0,
            confirmation: 0,
            end_to_end: 0,
        }
    }

    fn print_table(&self) {
        println!("\n=== LATENCY METRICS (ms) ===");
        println!("{:<30} | {:>10}", "Stage", "Duration");
        println!("{}", "-".repeat(45));
        println!("{:<30} | {:>10}", "RPC Ping (getHealth)", self.rpc_ping);
        println!("{:<30} | {:>10}", "Risk Gate Total", self.risk_gate_total);
        println!(
            "{:<30} | {:>10}",
            "  - Mint Freeze Check", self.risk_gate_mint_freeze
        );
        println!("{:<30} | {:>10}", "Sizing Compute", self.sizing_compute);
        println!("{:<30} | {:>10}", "Build Transaction", self.build_tx);
        println!(
            "{:<30} | {:>10}",
            "  - Fetch Blockhash", self.fetch_blockhash
        );
        println!(
            "{:<30} | {:>10}",
            "Signer Round-trip", self.signer_roundtrip
        );
        println!("{:<30} | {:>10}", "Routing Submit", self.routing_submit);
        println!("{:<30} | {:>10}", "Confirmation", self.confirmation);
        println!("{:<30} | {:>10}", "END-TO-END", self.end_to_end);
        println!("{}", "-".repeat(45));
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("=== E2E DEVNET INTEGRATION TEST ===");
    println!("Branch: e2e-devnet-test");
    println!("RPC: {}", DEVNET_RPC);
    println!("Pool: {}", DEVNET_RAYDIUM_POOL);
    println!();

    // SAFETY CHECK: Ensure we're using devnet
    if DEVNET_RPC.contains("mainnet") {
        error!("SAFETY VIOLATION: Attempting to use mainnet RPC!");
        error!("Aborting test.");
        return Err(anyhow::anyhow!("Mainnet RPC detected, aborting for safety"));
    }
    println!("SAFETY CHECK: Using devnet RPC ✓");

    // Check if test signer socket exists
    // If not, we'll skip the signer part and sign directly (documenting limitation)
    let use_signer = std::path::Path::new(TEST_SIGNER_SOCKET).exists();
    if use_signer {
        println!("Test signer socket found: {} ✓", TEST_SIGNER_SOCKET);
    } else {
        println!("Test signer socket NOT found: {}", TEST_SIGNER_SOCKET);
        println!("Will sign directly with test keypair (signer client not tested)");
        println!("To test signer client, start:");
        println!(
            "  SOLBOT_SIGNER_SOCKET={} SOLBOT_KEYPAIR_JSON={} cargo run --release -p solbot-signer",
            TEST_SIGNER_SOCKET, TEST_KEYPAIR_PATH
        );
    }

    // Generate or load test keypair
    let keypair = load_or_generate_test_keypair()?;
    let pubkey = keypair.pubkey();
    println!("Test keypair pubkey: {}", pubkey);

    // Request airdrop if balance is low
    let rpc = RpcClient::new(DEVNET_RPC.to_string());
    let balance = rpc.get_balance(&pubkey)?;
    println!("Current balance: {} SOL", balance as f64 / 1_000_000_000.0);

    if balance < 1_000_000_000 {
        println!("Requesting airdrop...");
        match rpc.request_airdrop(&pubkey, 2_000_000_000) {
            Ok(signature) => {
                println!("Airdrop requested: {}", signature);
                // Wait for airdrop confirmation
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                warn!(
                    "Airdrop failed: {}. Please request manually from https://faucet.solana.com",
                    e
                );
            }
        }
    }

    let mut metrics = LatencyMetrics::new();
    let test_start = Instant::now();

    // Step 1: RPC Ping
    println!("\n--- Step 1: RPC Ping ---");
    let ping_start = Instant::now();
    rpc.get_health()?;
    metrics.rpc_ping = ping_start.elapsed().as_millis() as u64;
    println!("Health check: OK ({}ms)", metrics.rpc_ping);

    // Step 2: Risk Gate Checks (all 4)
    println!("\n--- Step 2: Risk Gate Checks ---");
    let risk_gate_start = Instant::now();

    // Use WSOL mint (known to pass freeze authority check)
    let wsol_mint = "So11111111111111111111111111111111111111112";

    // 2.1: Mint Freeze Authority Check
    let mint_freeze_start = Instant::now();
    let mint_freeze_result = mint_freeze_authority::check(wsol_mint, DEVNET_RPC).await;
    metrics.risk_gate_mint_freeze = mint_freeze_start.elapsed().as_millis() as u64;
    println!(
        "Mint freeze check: {}ms - passed={}, reason={:?}",
        metrics.risk_gate_mint_freeze, mint_freeze_result.passed, mint_freeze_result.reason
    );

    // 2.2: LP Lock Check (using devnet Raydium pool)
    let lp_lock_start = Instant::now();
    let lp_lock_result =
        lp_lock_check::check(DEVNET_RAYDIUM_POOL, RAYDIUM_AMM_V4_PROGRAM, DEVNET_RPC).await;
    let lp_lock_duration = lp_lock_start.elapsed().as_millis() as u64;
    println!(
        "LP lock check: {}ms - passed={}, reason={:?}",
        lp_lock_duration, lp_lock_result.passed, lp_lock_result.reason
    );

    // 2.3: Token2022 Extensions Check (using WSOL - legacy token, so should be NotApplicable)
    let token2022_start = Instant::now();
    let token2022_config = solbot_strategy::risk_gate::RiskGateConfig::default();
    let token2022_result =
        token2022_extensions::check(wsol_mint, DEVNET_RPC, &token2022_config).await;
    let token2022_duration = token2022_start.elapsed().as_millis() as u64;
    println!(
        "Token2022 extensions check: {}ms - passed={}, reason={:?}",
        token2022_duration, token2022_result.passed, token2022_result.reason
    );

    // 2.4: Simulate Sell Check (skip for system transfer - only applicable for DEX swaps)
    println!("Simulate sell check: SKIPPED (not applicable to system transfer)");

    metrics.risk_gate_total = risk_gate_start.elapsed().as_millis() as u64;

    // Step 3: Sizing Compute
    println!("\n--- Step 3: Position Sizing ---");
    let sizing_start = Instant::now();
    let position_size_sol = 0.01; // 0.01 SOL for test
    let amount_in_lamports = (position_size_sol * 1_000_000_000.0) as u64;
    metrics.sizing_compute = sizing_start.elapsed().as_millis() as u64;
    println!(
        "Position size: {} SOL ({}ms)",
        position_size_sol, metrics.sizing_compute
    );

    // Step 4: Build Transaction
    println!("\n--- Step 4: Build Transaction ---");
    let build_start = Instant::now();

    // Fetch blockhash
    let blockhash_start = Instant::now();
    let blockhash = rpc.get_latest_blockhash()?;
    metrics.fetch_blockhash = blockhash_start.elapsed().as_millis() as u64;
    println!("Blockhash: {} ({}ms)", blockhash, metrics.fetch_blockhash);

    // Build a simple transfer transaction (for testing routing)
    let recipient = Pubkey::new_unique();
    let tx = Transaction::new_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &pubkey,
            &recipient,
            amount_in_lamports,
        )],
        Some(&pubkey),
    );
    let mut unsigned_tx = tx.clone();
    unsigned_tx.message.recent_blockhash = blockhash;

    metrics.build_tx = build_start.elapsed().as_millis() as u64;
    println!("Transaction built ({}ms)", metrics.build_tx);

    // Step 5: Sign via Signer Client (or directly if socket not available)
    println!("\n--- Step 5: Signer Client ---");

    let signed_tx = if use_signer {
        println!("Using real signer client via gRPC...");

        // Convert unsigned transaction to base64
        let unsigned_tx_bytes = bincode::serialize(&unsigned_tx)?;
        let unsigned_tx_base64 =
            base64::engine::general_purpose::STANDARD.encode(&unsigned_tx_bytes);

        // Create signer client
        let signer_client = SignerClient::new(TEST_SIGNER_SOCKET.to_string());

        // Build signing intent request
        // For system transfer test, use System Program ID in metadata to match actual instructions
        // Use unique pool_address to avoid sequence conflicts (signer uses mint or pool_address as context_key)
        let unique_pool = format!("e2e-test-{}", Utc::now().timestamp_millis());
        let sign_request = SignerIntentRequest {
            context_key: unique_pool.clone(),
            ipc_seq: 0, // Auto-assigned by signer client
            issued_at_unix_ms: Utc::now().timestamp_millis() as u64,
            intent_type: IntentType::Buy,
            unsigned_transaction_base64: unsigned_tx_base64.clone(),
            program_id: "11111111111111111111111111111111".to_string(), // System Program (matches actual instructions)
            dex_program_id: "11111111111111111111111111111111".to_string(),
            max_size_lamports: amount_in_lamports,
            hard_cap_bps: 1000,
            destination_addresses: vec![recipient.to_string()],
            mint: "".to_string(), // Empty so signer uses pool_address as context_key
            pool_address: unique_pool.clone(),
            position_id: None,
            amount_or_pct: position_size_sol,
        };

        // PRECISE TIMING: Measure ONLY the gRPC round-trip
        let grpc_start = Instant::now();
        let response = signer_client.sign_intent(sign_request).await?;
        let grpc_duration_us = grpc_start.elapsed().as_micros();

        if response.status != VerdictStatus::Accepted {
            let reason = response
                .rejection_reason
                .unwrap_or_else(|| "Unknown".to_string());
            error!("Signer rejected intent: {}", reason);
            return Err(anyhow::anyhow!("Signer rejection: {}", reason));
        }

        // Decode signed transaction from response
        let signed_base64 = response
            .signed_transaction_base64
            .ok_or_else(|| anyhow::anyhow!("No signed transaction returned"))?;
        let signed_bytes = base64::engine::general_purpose::STANDARD.decode(&signed_base64)?;
        let signed_tx_decoded: Transaction = bincode::deserialize(&signed_bytes)?;

        println!(
            "Signer client gRPC round-trip: {}µs ({}ms)",
            grpc_duration_us,
            grpc_duration_us as f64 / 1000.0
        );
        signed_tx_decoded
    } else {
        println!("Signing directly (signer client not available)");
        let mut direct_signed = unsigned_tx.clone();
        direct_signed.try_sign(&[&keypair], blockhash)?;
        direct_signed
    };

    let signer_start = Instant::now();
    metrics.signer_roundtrip = signer_start.elapsed().as_millis() as u64;
    println!(
        "Transaction signed - {}",
        if use_signer {
            "via signer client gRPC"
        } else {
            "direct signing"
        }
    );

    // Step 6: Submit to Devnet
    println!("\n--- Step 6: Routing Submit ---");
    println!("SUBMITTING TO: {}", DEVNET_RPC);

    if DEVNET_RPC.contains("mainnet") {
        error!("SAFETY VIOLATION: Attempting to submit to mainnet!");
        return Err(anyhow::anyhow!("Mainnet submission detected, aborting"));
    }

    let submit_start = Instant::now();
    let signature = rpc.send_transaction_with_config(
        &signed_tx,
        RpcSendTransactionConfig {
            skip_preflight: false,
            ..Default::default()
        },
    )?;
    metrics.routing_submit = submit_start.elapsed().as_millis() as u64;
    println!(
        "Transaction submitted: {} ({}ms)",
        signature, metrics.routing_submit
    );

    // Step 7: Confirmation
    println!("\n--- Step 7: Confirmation ---");
    let confirm_start = Instant::now();

    let mut confirmed = false;
    let max_wait = Duration::from_secs(30);
    let poll_interval = Duration::from_millis(500);

    while confirm_start.elapsed() < max_wait {
        if let Some(status) = rpc.get_signature_status(&signature)? {
            if status.is_ok() {
                confirmed = true;
                break;
            }
        }
        tokio::time::sleep(poll_interval).await;
    }

    metrics.confirmation = confirm_start.elapsed().as_millis() as u64;
    metrics.end_to_end = test_start.elapsed().as_millis() as u64;

    if confirmed {
        println!("Transaction CONFIRMED ✓ ({}ms)", metrics.confirmation);
        println!(
            "Explorer: https://explorer.solana.com/tx/{}?cluster=devnet",
            signature
        );
    } else {
        println!("Transaction NOT confirmed within timeout");
    }

    // Print metrics
    metrics.print_table();

    println!("\n=== TEST COMPLETED ===");
    Ok(())
}

fn load_or_generate_test_keypair() -> Result<Keypair> {
    if std::path::Path::new(TEST_KEYPAIR_PATH).exists() {
        println!("Loading existing test keypair from {}", TEST_KEYPAIR_PATH);
        let json_str = std::fs::read_to_string(TEST_KEYPAIR_PATH)?;
        let bytes: Vec<u8> = serde_json::from_str(&json_str)?;
        let keypair = Keypair::from_bytes(&bytes)?;
        Ok(keypair)
    } else {
        println!("Generating new test keypair...");
        let keypair = Keypair::new();
        let bytes = keypair.to_bytes();
        let json_array = serde_json::to_string(&bytes.to_vec())?;
        std::fs::write(TEST_KEYPAIR_PATH, json_array)?;
        println!("Test keypair saved to {}", TEST_KEYPAIR_PATH);
        Ok(keypair)
    }
}
