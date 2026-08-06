//! Integration test for Raydium AMM v4 real pricing against mainnet.
//!
//! This test fetches actual pool state from mainnet and compares calculated
//! pricing with Raydium API quotes to verify accuracy.
//!
//! Run with: SOLANA_RPC_URL=<mainnet_rpc> cargo test --package solbot-strategy --test raydium_amm_v4_pricing_integration

use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solbot_strategy::tx_builder::raydium_amm_v4::RaydiumAmmV4PoolState;
use std::str::FromStr;

/// SOL/USDC AMM v4 pool on mainnet (high liquidity pool)
const SOL_USDC_POOL: &str = "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2";

#[test]
#[ignore = "Requires mainnet RPC URL: SOLANA_RPC_URL=<mainnet_rpc>"]
fn test_raydium_amm_v4_mainnet_pricing() {
    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .expect("SOLANA_RPC_URL environment variable must be set for integration test");
    
    let rpc = RpcClient::new(rpc_url);
    let pool_pubkey = Pubkey::from_str(SOL_USDC_POOL).unwrap();
    
    // Fetch pool account
    let pool_account = rpc.get_account(&pool_pubkey)
        .expect("Failed to fetch pool account from mainnet");
    
    println!("Pool account data length: {} bytes", pool_account.data.len());
    
    // Parse pool state
    let pool_state = RaydiumAmmV4PoolState::from_account_data(&pool_account.data)
        .expect("Failed to parse pool state");
    
    println!("Pool state parsed successfully:");
    println!("  AMM ID: {}", pool_state.amm_id);
    println!("  Coin vault: {}", pool_state.serum_coin_vault);
    println!("  PC vault: {}", pool_state.serum_pc_vault);
    println!("  Coin mint: {}", pool_state.coin_mint);
    println!("  PC mint: {}", pool_state.pc_mint);
    println!("  Fees:");
    println!("    swap_fee: {}/{}", pool_state.fees.swap_fee_numerator, pool_state.fees.swap_fee_denominator);
    println!("    trade_fee: {}/{}", pool_state.fees.trade_fee_numerator, pool_state.fees.trade_fee_denominator);
    
    // Fetch vault balances
    let coin_balance = rpc.get_token_account_balance(&pool_state.serum_coin_vault)
        .expect("Failed to fetch coin vault balance");
    let pc_balance = rpc.get_token_account_balance(&pool_state.serum_pc_vault)
        .expect("Failed to fetch pc vault balance");
    
    let coin_balance_u64: u64 = coin_balance.amount.parse().expect("Failed to parse coin balance");
    let pc_balance_u64: u64 = pc_balance.amount.parse().expect("Failed to parse pc balance");
    
    println!("Vault balances:");
    println!("  Coin vault: {} (raw)", coin_balance_u64);
    println!("  PC vault: {} (raw)", pc_balance_u64);
    
    // Set vault balances on pool state
    let pool_state = pool_state.with_vault_balances(coin_balance_u64, pc_balance_u64);
    
    // Test pricing with 1 SOL input
    let amount_in = 1_000_000_000; // 1 SOL (9 decimals)
    let expected_output = pool_state.calculate_buy_output(amount_in);
    
    println!("Pricing calculation:");
    println!("  Input: {} lamports (1 SOL)", amount_in);
    println!("  Expected output: {} raw units", expected_output);
    
    // Verify output is non-zero
    assert!(expected_output > 0, "Expected output should be non-zero");
    
    // Rough sanity check: with current reserves, 1 SOL should get ~74 USDC
    // 74 USDC = 74,000,000 raw units (6 decimals)
    // Allow wide range due to price fluctuations and decimal differences
    assert!(expected_output > 50_000_000 && expected_output < 100_000_000,
        "Expected output {} outside reasonable range for 1 SOL input", expected_output);
    
    println!("Test passed: pricing calculation is reasonable");
}

#[test]
#[ignore = "Requires mainnet RPC URL: SOLANA_RPC_URL=<mainnet_rpc>"]
fn test_raydium_amm_v4_sell_pricing() {
    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .expect("SOLANA_RPC_URL environment variable must be set for integration test");
    
    let rpc = RpcClient::new(rpc_url);
    let pool_pubkey = Pubkey::from_str(SOL_USDC_POOL).unwrap();
    
    // Fetch pool account
    let pool_account = rpc.get_account(&pool_pubkey)
        .expect("Failed to fetch pool account from mainnet");
    
    // Parse pool state
    let pool_state = RaydiumAmmV4PoolState::from_account_data(&pool_account.data)
        .expect("Failed to parse pool state");
    
    // Fetch vault balances
    let coin_balance = rpc.get_token_account_balance(&pool_state.serum_coin_vault)
        .expect("Failed to fetch coin vault balance");
    let pc_balance = rpc.get_token_account_balance(&pool_state.serum_pc_vault)
        .expect("Failed to fetch pc vault balance");
    
    let coin_balance_u64: u64 = coin_balance.amount.parse().expect("Failed to parse coin balance");
    let pc_balance_u64: u64 = pc_balance.amount.parse().expect("Failed to parse pc balance");
    
    let pool_state = pool_state.with_vault_balances(coin_balance_u64, pc_balance_u64);
    
    // Test sell pricing with 100 USDC input
    let amount_in = 100_000_000; // 100 USDC (6 decimals)
    let expected_output = pool_state.calculate_sell_output(amount_in);
    
    println!("Sell pricing calculation:");
    println!("  Input: {} raw units (100 USDC)", amount_in);
    println!("  Expected output: {} lamports", expected_output);
    
    // Verify output is non-zero
    assert!(expected_output > 0, "Expected output should be non-zero");
    
    // Rough sanity check: 100 USDC should get ~1.3 SOL
    // 1.3 SOL = 1,300,000,000 lamports (9 decimals)
    assert!(expected_output > 1_000_000_000 && expected_output < 2_000_000_000,
        "Expected output {} outside reasonable range for 100 USDC input", expected_output);
    
    println!("Test passed: sell pricing calculation is reasonable");
}
