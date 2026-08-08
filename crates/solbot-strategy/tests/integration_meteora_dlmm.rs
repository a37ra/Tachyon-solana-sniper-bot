//! Integration test for Meteora DLMM swap instruction builder.

use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use solbot_strategy::tx_builder::{
    meteora_dlmm,
    meteora_dlmm::{DlmmPoolState, METEORA_DLMM_PROGRAM},
    SwapDirection, SwapParams,
};

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_meteora_dlmm_swap_instruction_structure() {
    let wallet = Keypair::new();
    let mint_x = Pubkey::new_unique();
    let mint_y = Pubkey::new_unique();
    let pool_id = Pubkey::new_unique();
    let reserve_x = Pubkey::new_unique();
    let reserve_y = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let dummy_pool = DlmmPoolState {
        lb_pair: pool_id,
        bin_array_bitmap_extension: None,
        token_x_mint: mint_x,
        token_y_mint: mint_y,
        reserve_x,
        reserve_y,
        oracle,
        active_id: 1000,
        reserve_x_balance: 0,
        reserve_y_balance: 0,
        bin_step: 1,
    };

    let params = SwapParams {
        wallet: wallet.pubkey(),
        mint: mint_x,
        pool_address: pool_id,
        dex_program_id: METEORA_DLMM_PROGRAM,
        amount_in: 1_000_000_000,
        minimum_amount_out: 500_000_000,
        slippage_bps: 500,
        direction: SwapDirection::Buy,
    };

    let swap_instructions = meteora_dlmm::build_swap(&params, &dummy_pool).unwrap();

    assert!(!swap_instructions.setup_instructions.is_empty());
    assert_eq!(swap_instructions.swap_instructions.len(), 1);
    assert!(!swap_instructions.cleanup_instructions.is_empty());

    let swap_ix = &swap_instructions.swap_instructions[0];
    assert_eq!(swap_ix.program_id, METEORA_DLMM_PROGRAM);
    assert!(swap_ix.accounts.len() >= 15);

    let account_pubkeys: Vec<_> = swap_ix.accounts.iter().map(|a| a.pubkey).collect();
    assert!(account_pubkeys.contains(&pool_id));
    assert!(account_pubkeys.contains(&wallet.pubkey()));

    println!("✓ Meteora DLMM swap instruction structure validated");
}

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_meteora_dlmm_sell_swap_structure() {
    let wallet = Keypair::new();
    let mint_x = Pubkey::new_unique();
    let mint_y = Pubkey::new_unique();
    let pool_id = Pubkey::new_unique();
    let reserve_x = Pubkey::new_unique();
    let reserve_y = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    let dummy_pool = DlmmPoolState {
        lb_pair: pool_id,
        bin_array_bitmap_extension: None,
        token_x_mint: mint_x,
        token_y_mint: mint_y,
        reserve_x,
        reserve_y,
        oracle,
        active_id: 1000,
        reserve_x_balance: 0,
        reserve_y_balance: 0,
        bin_step: 1,
    };

    let params = SwapParams {
        wallet: wallet.pubkey(),
        mint: mint_x,
        pool_address: pool_id,
        dex_program_id: METEORA_DLMM_PROGRAM,
        amount_in: 100_000_000,
        minimum_amount_out: 0,
        slippage_bps: 500,
        direction: SwapDirection::Sell,
    };

    let swap_instructions = meteora_dlmm::build_swap(&params, &dummy_pool).unwrap();
    assert!(!swap_instructions.setup_instructions.is_empty());
    assert_eq!(swap_instructions.swap_instructions.len(), 1);

    println!("✓ Meteora DLMM sell swap structure validated");
}

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_meteora_dlmm_bin_array_derivation() {
    let pool_id = Pubkey::new_unique();

    let bin_index = 1000;
    let bin_array = meteora_dlmm::derive_bin_array_address(&pool_id, bin_index).unwrap();

    assert_ne!(bin_array, Pubkey::default());
    assert_ne!(bin_array, pool_id);

    println!("✓ Meteora DLMM bin array derivation validated");
}

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_meteora_dlmm_different_active_ids() {
    let wallet = Keypair::new();
    let mint_x = Pubkey::new_unique();
    let mint_y = Pubkey::new_unique();
    let pool_id = Pubkey::new_unique();
    let reserve_x = Pubkey::new_unique();
    let reserve_y = Pubkey::new_unique();
    let oracle = Pubkey::new_unique();

    for active_id in [0, 1000, 5000, -1000] {
        let dummy_pool = DlmmPoolState {
            lb_pair: pool_id,
            bin_array_bitmap_extension: None,
            token_x_mint: mint_x,
            token_y_mint: mint_y,
            reserve_x,
            reserve_y,
            oracle,
            active_id,
            reserve_x_balance: 0,
            reserve_y_balance: 0,
            bin_step: 1,
        };

        let params = SwapParams {
            wallet: wallet.pubkey(),
            mint: mint_x,
            pool_address: pool_id,
            dex_program_id: METEORA_DLMM_PROGRAM,
            amount_in: 1_000_000_000,
            minimum_amount_out: 0,
            slippage_bps: 500,
            direction: SwapDirection::Buy,
        };

        let swap_instructions = meteora_dlmm::build_swap(&params, &dummy_pool).unwrap();
        assert!(!swap_instructions.swap_instructions.is_empty());
    }

    println!("✓ Meteora DLMM different active IDs validated");
}
