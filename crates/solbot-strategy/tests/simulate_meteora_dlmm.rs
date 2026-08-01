//! SimulateTransaction test for Meteora DLMM on devnet.
//!
//! This test builds a Meteora DLMM swap transaction and simulates it
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
    meteora_dlmm::{build_swap as meteora_dlmm_build_swap, DlmmPoolState},
    blockhash_cache::BlockhashCache,
};

/// Devnet RPC URL
const DEVNET_RPC: &str = "https://api.devnet.solana.com";

/// Meteora DLMM program ID on devnet
const METEORA_DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp";

/// Test amount in SOL (0.01 SOL = 10,000,000 lamports)
const TEST_AMOUNT_SOL: u64 = 10_000_000;

/// Slippage tolerance (5% = 500 bps)
const TEST_SLIPPAGE_BPS: u32 = 500;

#[tokio::test]
async fn test_meteora_dlmm_simulate_transaction() -> Result<()> {
    println!("🧪 Testing Meteora DLMM simulateTransaction on devnet");
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

    // Mock DLMM pool state (in production, this would be fetched from RPC)
    let lb_pair = Pubkey::from_str("7qbRF6YsyMrLdN7f8XJ5qS5P1q1XqXqXqXqXqXqXqXqX").unwrap(); // Example pool
    let dlmm_pool = DlmmPoolState {
        lb_pair,
        bin_array_bitmap_extension: None,
        token_x_mint: Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap(), // WSOL
        token_y_mint: Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap(), // USDC
        reserve_x: Pubkey::new_unique(),
        reserve_y: Pubkey::new_unique(),
        oracle: Pubkey::new_unique(),
        active_id: 1000,
    };

    // Create swap parameters
    let mint_b = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
    let params = SwapParams {
        mint: mint_b,
        pool_address: lb_pair,
        dex_program_id: Pubkey::from_str(METEORA_DLMM_PROGRAM).unwrap(),
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

    // Build DLMM swap instructions
    let swap_instructions = meteora_dlmm_build_swap(&params, &dlmm_pool)
        .context("Failed to build Meteora DLMM swap instructions")?;

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
async fn test_meteora_dlmm_bin_array_calculation() -> Result<()> {
    println!("🧪 Testing Meteora DLMM bin array calculation");
    println!("==============================================");

    // Test bin array index calculation
    let bin_id = 1000;
    
    let bin_array_index = solbot_strategy::tx_builder::meteora_dlmm::bin_array_index_from_bin_id(bin_id);
    
    println!("✅ Bin array index calculation:");
    println!("  Bin ID: {}", bin_id);
    println!("  Bin array index: {}", bin_array_index);

    // Verify the calculation is correct (Meteora uses 70 bins per array)
    let expected_index = bin_id as i64 / 70;
    assert_eq!(bin_array_index, expected_index, "Bin array index calculation incorrect");

    println!("✅ Bin array calculation verified");
    Ok(())
}
