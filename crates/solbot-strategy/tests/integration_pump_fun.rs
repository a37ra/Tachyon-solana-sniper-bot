//! Integration test for Pump.fun bonding curve swap instruction builder.

use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use solbot_strategy::tx_builder::{
    pump_fun,
    pump_fun::{PumpFunBondingCurve, PUMP_FUN_PROGRAM},
    SwapDirection, SwapParams,
};

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_pump_fun_swap_instruction_structure() {
    let wallet = Keypair::new();
    let mint = Pubkey::new_unique();
    let bonding_curve = Pubkey::new_unique();
    let associated_bonding_curve = Pubkey::new_unique();

    let dummy_curve = PumpFunBondingCurve {
        bonding_curve,
        mint,
        associated_bonding_curve,
        virtual_token_reserves: 1_000_000_000,
        virtual_sol_reserves: 30_000_000_000,
        real_token_reserves: 800_000_000,
        real_sol_reserves: 30_000_000_000,
        token_total_supply: 1_000_000_000,
        complete: false,
    };

    let params = SwapParams {
        wallet: wallet.pubkey(),
        mint,
        pool_address: bonding_curve,
        dex_program_id: PUMP_FUN_PROGRAM,
        amount_in: 1_000_000_000,
        minimum_amount_out: 500_000_000,
        slippage_bps: 500,
        direction: SwapDirection::Buy,
    };

    let swap_instructions = pump_fun::build_swap(&params, &dummy_curve).unwrap();

    assert!(!swap_instructions.setup_instructions.is_empty());
    assert_eq!(swap_instructions.swap_instructions.len(), 1);
    // Cleanup may be empty for some DEX configurations

    let swap_ix = &swap_instructions.swap_instructions[0];
    assert_eq!(swap_ix.program_id, PUMP_FUN_PROGRAM);
    assert!(swap_ix.accounts.len() >= 10);

    let account_pubkeys: Vec<_> = swap_ix.accounts.iter().map(|a| a.pubkey).collect();
    assert!(account_pubkeys.contains(&bonding_curve));
    assert!(account_pubkeys.contains(&wallet.pubkey()));

    println!("✓ Pump.fun swap instruction structure validated");
}

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_pump_fun_sell_swap_structure() {
    let wallet = Keypair::new();
    let mint = Pubkey::new_unique();
    let bonding_curve = Pubkey::new_unique();
    let associated_bonding_curve = Pubkey::new_unique();

    let dummy_curve = PumpFunBondingCurve {
        bonding_curve,
        mint,
        associated_bonding_curve,
        virtual_token_reserves: 1_000_000_000,
        virtual_sol_reserves: 30_000_000_000,
        real_token_reserves: 800_000_000,
        real_sol_reserves: 30_000_000_000,
        token_total_supply: 1_000_000_000,
        complete: false,
    };

    let params = SwapParams {
        wallet: wallet.pubkey(),
        mint,
        pool_address: bonding_curve,
        dex_program_id: PUMP_FUN_PROGRAM,
        amount_in: 100_000_000,
        minimum_amount_out: 0,
        slippage_bps: 500,
        direction: SwapDirection::Sell,
    };

    let swap_instructions = pump_fun::build_swap(&params, &dummy_curve).unwrap();
    assert!(!swap_instructions.setup_instructions.is_empty());
    assert_eq!(swap_instructions.swap_instructions.len(), 1);

    println!("✓ Pump.fun sell swap structure validated");
}

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_pump_fun_complete_pool() {
    let wallet = Keypair::new();
    let mint = Pubkey::new_unique();
    let bonding_curve = Pubkey::new_unique();
    let associated_bonding_curve = Pubkey::new_unique();

    let completed_curve = PumpFunBondingCurve {
        bonding_curve,
        mint,
        associated_bonding_curve,
        virtual_token_reserves: 1_000_000_000,
        virtual_sol_reserves: 30_000_000_000,
        real_token_reserves: 1_000_000_000,
        real_sol_reserves: 30_000_000_000,
        token_total_supply: 1_000_000_000,
        complete: true,
    };

    let params = SwapParams {
        wallet: wallet.pubkey(),
        mint,
        pool_address: bonding_curve,
        dex_program_id: PUMP_FUN_PROGRAM,
        amount_in: 1_000_000_000,
        minimum_amount_out: 0,
        slippage_bps: 500,
        direction: SwapDirection::Buy,
    };

    // Completed pools should reject with an error
    let result = pump_fun::build_swap(&params, &completed_curve);
    assert!(
        result.is_err(),
        "Completed bonding curve should reject swap"
    );

    println!("✓ Pump.fun completed pool correctly rejected");
}
