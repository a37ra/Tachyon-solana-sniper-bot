//! SimulateTransaction test for Raydium CLMM on devnet.
//!
//! This test builds a Raydium CLMM swap transaction and simulates it
//! on devnet to verify the transaction would execute successfully.

use std::str::FromStr;
use std::sync::Arc;
use anyhow::{Context, Result};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSimulateTransactionConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::transaction::Transaction;
use solana_signer::Signer;

use solbot_strategy::tx_builder::{
    SwapDirection, SwapParams,
    raydium_clmm::{build_swap as raydium_clmm_build_swap, ClmmPoolState},
    blockhash_cache::BlockhashCache,
};

/// Devnet RPC URL
const DEVNET_RPC: &str = "https://api.devnet.solana.com";

/// Raydium CLMM program ID on devnet
const RAYDIUM_CLMM_PROGRAM: &str = "CAMMCzo5yh8jvTJ8dougZL99t79ta5caPbGypFn4xQPU";

/// Test amount in SOL (0.01 SOL = 10,000,000 lamports)
const TEST_AMOUNT_SOL: u64 = 10_000_000;

/// Slippage tolerance (5% = 500 bps)
const TEST_SLIPPAGE_BPS: u32 = 500;

#[tokio::test]
async fn test_raydium_clmm_simulate_transaction() -> Result<()> {
    println!("🧪 Testing Raydium CLMM simulateTransaction on devnet");
    println!("=====================================================");

    // Connect to devnet
    let rpc_client = Arc::new(RpcClient::new(DEVNET_RPC));
    println!("✅ Connected to devnet RPC");

    // Create a test wallet
    let wallet = Keypair::new();
    let wallet_pubkey = wallet.pubkey();
    println!("🔑 Test wallet: {}", wallet_pubkey);

    // Get recent blockhash
    let blockhash_cache = BlockhashCache::new(rpc_client.clone()).await?;
    let (recent_blockhash, _) = blockhash_cache.get_blockhash().await?;
    println!("✅ Blockhash: {}", recent_blockhash);

    // Mock CLMM pool state (in production, this would be fetched from RPC)
    let pool_id = Pubkey::from_str("8sLbNZAtiBQSjAd4hCk15zRgBqFk4ZHvZfTfVJ2v7vM").unwrap(); // Example pool
    let clmm_pool = ClmmPoolState {
        pool_id,
        amm_config: Pubkey::new_unique(),
        owner: Pubkey::new_unique(),
        token_mint_0: Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap(), // WSOL
        token_mint_1: Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap(), // USDC
        token_vault_0: Pubkey::new_unique(),
        token_vault_1: Pubkey::new_unique(),
        observation_key: Pubkey::new_unique(),
        tick_array_bitmap_extension: None,
        tick_current: 0,
        tick_spacing: 64,
    };

    // Create swap parameters
    let mint_b = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
    let params = SwapParams {
        mint: mint_b,
        pool_address: pool_id,
        dex_program_id: Pubkey::from_str(RAYDIUM_CLMM_PROGRAM).unwrap(),
        wallet: wallet_pubkey,
        direction: SwapDirection::Buy,
        amount_in: TEST_AMOUNT_SOL,
        minimum_amount_out: 0,
        slippage_bps: TEST_SLIPPAGE_BPS,
    };

    println!("📊 Swap parameters:");
    println!("  Mint: {}", params.mint);
    println!("  Pool: {}", params.pool_address);
    println!("  Direction: {:?}", params.direction);
    println!("  Amount: {} lamports", params.amount_in);

    // Build CLMM swap instructions
    let swap_instructions = raydium_clmm_build_swap(&params, &clmm_pool, &pool_id)
        .context("Failed to build Raydium CLMM swap instructions")?;

    println!("✅ Built swap instructions");
    println!("  Setup: {}", swap_instructions.setup_instructions.len());
    println!("  Swap: {}", swap_instructions.swap_instructions.len());
    println!("  Cleanup: {}", swap_instructions.cleanup_instructions.len());

    // Build unsigned transaction
    let (unsigned_tx, tx_base64) = solbot_strategy::tx_builder::build_unsigned_transaction(
        swap_instructions,
        200_000, // Compute units
        1000,    // Priority fee micro-lamports
        &wallet_pubkey,
        recent_blockhash,
    ).context("Failed to build unsigned transaction")?;

    println!("✅ Transaction built:");
    println!("  Data length: {} bytes", tx_base64.len());
    println!("  Instructions: {}", unsigned_tx.message.instructions.len());

    // Sign the transaction
    let signed_tx = Transaction::new(
        &[&wallet],
        unsigned_tx.message,
        recent_blockhash,
    );

    println!("✅ Transaction signed");

    // Simulate the transaction
    println!("🔬 Simulating transaction...");
    let config = RpcSimulateTransactionConfig {
        sig_verify: false,
        replace_recent_blockhash: false,
        commitment: Some(CommitmentConfig::confirmed()),
        ..Default::default()
    };

    let simulation_result = rpc_client
        .simulate_transaction_with_config(&signed_tx, config)
        .map_err(|e| anyhow::anyhow!("Transaction simulation failed: {}", e))?;

    println!("✅ Simulation completed");
    println!("📊 Simulation results:");
    println!("  Units consumed: {}", simulation_result.value.units_consumed.unwrap_or(0));
    
    if let Some(logs) = &simulation_result.value.logs {
        println!("  Log lines: {}", logs.len());
        for (i, log) in logs.iter().take(10).enumerate() {
            println!("    [{}]: {}", i, log);
        }
    }

    if let Some(err) = &simulation_result.value.err {
        println!("❌ Simulation error: {}", err);
        anyhow::bail!("Transaction simulation failed: {}", err);
    }

    println!("✅ Transaction simulation successful!");
    Ok(())
}

#[tokio::test]
async fn test_raydium_clmm_tick_array_calculation() -> Result<()> {
    println!("🧪 Testing Raydium CLMM tick array calculation");
    println!("==============================================");

    // Test tick array start index calculation
    let tick_current = 1000;
    let tick_spacing = 64;
    
    let tick_array_index = solbot_strategy::tx_builder::raydium_clmm::tick_array_start_index(
        tick_current,
        tick_spacing,
    );
    
    println!("✅ Tick array start index calculation:");
    println!("  Tick current: {}", tick_current);
    println!("  Tick spacing: {}", tick_spacing);
    println!("  Tick array index: {}", tick_array_index);

    // Verify the calculation is correct
    let expected_index = tick_current / (tick_spacing as i32 * 60);
    assert_eq!(tick_array_index, expected_index, "Tick array index calculation incorrect");

    println!("✅ Tick array calculation verified");
    Ok(())
}
