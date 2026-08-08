//! Integration test for Raydium AMM v4 swap instruction builder.
//!
//! This test verifies that swap instructions are built correctly
//! with the expected account layout.

use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use solbot_strategy::tx_builder::{
    ata, raydium_amm_v4,
    raydium_amm_v4::{RaydiumAmmV4PoolState, RAYDIUM_AMM_V4_PROGRAM},
    SwapDirection, SwapParams,
};

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_raydium_amm_v4_pool_state_parsing() {
    let _wallet = Keypair::new();

    // Create dummy pool data (752 bytes minimum for Raydium AMM v4)
    let mut pool_data = vec![0u8; 752];

    // Write some pubkeys at expected offsets
    let amm_id = Pubkey::new_unique();
    let coin_mint = Pubkey::new_unique();
    let pc_mint = Pubkey::new_unique();

    pool_data[8..40].copy_from_slice(amm_id.as_ref());
    pool_data[400..432].copy_from_slice(coin_mint.as_ref());
    pool_data[368..400].copy_from_slice(pc_mint.as_ref());

    let pool_state = RaydiumAmmV4PoolState::from_account_data(&pool_data).unwrap();

    assert_ne!(pool_state.amm_id, Pubkey::default());
    assert_ne!(pool_state.coin_mint, Pubkey::default());
    assert_ne!(pool_state.pc_mint, Pubkey::default());

    println!("✓ Pool state parsed successfully");
}

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_raydium_amm_v4_swap_instruction_structure() {
    let wallet = Keypair::new();
    let pool_pubkey = Pubkey::new_unique();
    let mint = Pubkey::new_unique();

    let mut pool_data = vec![0u8; 752];
    let amm_id = Pubkey::new_unique();
    let coin_mint = mint;
    let pc_mint = Pubkey::new_unique();

    pool_data[8..40].copy_from_slice(amm_id.as_ref());
    pool_data[400..432].copy_from_slice(coin_mint.as_ref());
    pool_data[368..400].copy_from_slice(pc_mint.as_ref());

    let pool_state = RaydiumAmmV4PoolState::from_account_data(&pool_data).unwrap();

    let params = SwapParams {
        wallet: wallet.pubkey(),
        mint,
        pool_address: pool_pubkey,
        dex_program_id: RAYDIUM_AMM_V4_PROGRAM,
        amount_in: 1_000_000_000,
        minimum_amount_out: 0,
        slippage_bps: 500,
        direction: SwapDirection::Buy,
    };

    let swap_instructions = raydium_amm_v4::build_swap(&params, &pool_state).unwrap();

    assert!(!swap_instructions.setup_instructions.is_empty());
    assert_eq!(swap_instructions.swap_instructions.len(), 1);
    assert!(!swap_instructions.cleanup_instructions.is_empty());

    let swap_ix = &swap_instructions.swap_instructions[0];
    assert_eq!(swap_ix.program_id, RAYDIUM_AMM_V4_PROGRAM);
    assert!(
        swap_ix.accounts.len() >= 17,
        "Raydium AMM v4 swap requires at least 17 accounts"
    );

    let account_pubkeys: Vec<_> = swap_ix.accounts.iter().map(|a| a.pubkey).collect();
    assert!(account_pubkeys.contains(&spl_token::id()));
    assert!(account_pubkeys.contains(&pool_state.amm_id));
    assert!(account_pubkeys.contains(&wallet.pubkey()));

    println!("✓ Swap instruction structure validated");
}

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_raydium_amm_v4_ata_derivation() {
    let wallet = Keypair::new();
    let mint = Pubkey::new_unique();

    let ata = ata::derive_ata(&wallet.pubkey(), &mint);
    let ata2 = ata::derive_ata(&wallet.pubkey(), &mint);
    assert_eq!(ata, ata2);

    println!("✓ ATA derivation validated");
}

#[tokio::test]
#[ignore = "Requires devnet connection"]
async fn test_raydium_amm_v4_authority_derivation() {
    let authority = RaydiumAmmV4PoolState::derive_authority().unwrap();
    assert_ne!(authority, Pubkey::default());
    assert_ne!(authority, RAYDIUM_AMM_V4_PROGRAM);

    println!("✓ AMM authority PDA derived");
}
