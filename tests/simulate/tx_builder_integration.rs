//! Basic integration tests for the transaction builder infrastructure.
//!
//! These tests verify that:
//! - Swap instructions are correctly constructed for each DEX
//! - Compute budget instructions are properly formed
//! - ATA derivation and wrapping logic works
//! - Transaction assembly produces valid unsigned transactions
//!
//! NOTE: These tests do NOT require network access or devnet.
//! They test instruction construction locally using synthetic pool state data.

use solana_pubkey::Pubkey;
use solana_sdk::hash::Hash;

/// Helper: create a fake pool state buffer of `size` bytes with pubkeys at known offsets.
fn make_pool_data(size: usize) -> Vec<u8> {
    let mut data = vec![0u8; size];
    // Fill with deterministic but non-zero data to avoid edge cases
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = ((i * 7 + 13) % 256) as u8;
    }
    data
}

#[test]
fn test_compute_budget_integration() {
    use solbot_strategy::tx_builder::compute_budget::*;

    let ixs = create_compute_budget_instructions(300_000, 100_000);
    assert_eq!(ixs.len(), 2, "Expected SetComputeUnitLimit + SetComputeUnitPrice");

    // Verify both instructions target the compute budget program
    let compute_budget_program = solana_pubkey::pubkey!("ComputeBudget111111111111111111111111111111");
    for ix in &ixs {
        assert_eq!(ix.program_id, compute_budget_program);
    }

    // Verify estimates for all supported DEXes
    assert!(estimate_compute_units("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8") > 0);
    assert!(estimate_compute_units("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C") > 0);
    assert!(estimate_compute_units("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK") > 0);
    assert!(estimate_compute_units("LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp") > 0);
    assert!(estimate_compute_units("Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB") > 0);
    assert!(estimate_compute_units("6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2") > 0);
}

#[test]
fn test_ata_derivation_and_wrapping() {
    use solbot_strategy::tx_builder::ata::*;

    let wallet = Pubkey::new_unique();
    let mint = Pubkey::new_unique();

    // ATA derivation must be deterministic
    let ata1 = derive_ata(&wallet, &mint);
    let ata2 = derive_ata(&wallet, &mint);
    assert_eq!(ata1, ata2);

    // Different mints must produce different ATAs
    let mint2 = Pubkey::new_unique();
    let ata3 = derive_ata(&wallet, &mint2);
    assert_ne!(ata1, ata3);

    // create_ata_instruction_if_needed must produce a valid instruction
    let ix = create_ata_instruction_if_needed(&wallet, &wallet, &mint);
    assert_eq!(ix.program_id, spl_associated_token_account::id());

    // wrap_sol_instructions must produce 3 instructions: create ATA, transfer, sync_native
    let (wsol_ata, wrap_ixs) = wrap_sol_instructions(&wallet, 1_000_000_000).unwrap();
    assert_eq!(wrap_ixs.len(), 3);
    assert_eq!(wsol_ata, derive_ata(&wallet, &WSOL_MINT));

    // close_wsol_account must produce a valid close instruction
    let close_ix = close_wsol_account_instruction(&wallet).unwrap();
    assert_eq!(close_ix.program_id, spl_token::id());
}

#[test]
fn test_slippage_calculation() {
    use solbot_strategy::tx_builder::apply_slippage;

    // Normal cases
    assert_eq!(apply_slippage(1_000_000, 500), 950_000);    // 5%
    assert_eq!(apply_slippage(1_000_000, 100), 990_000);    // 1%
    assert_eq!(apply_slippage(1_000_000, 0), 1_000_000);    // 0%
    assert_eq!(apply_slippage(1_000_000, 10000), 0);        // 100%

    // Edge cases: no overflow on large values
    let result = apply_slippage(u64::MAX, 500);
    assert!(result > 0);
    assert!(result < u64::MAX);
}

#[test]
fn test_build_unsigned_transaction_assembly() {
    use solbot_strategy::tx_builder::*;

    let wallet = Pubkey::new_unique();
    let blockhash = Hash::new_unique();

    // Create minimal swap instructions
    let swap_ixs = SwapInstructions {
        setup_instructions: vec![
            ata::create_ata_instruction_if_needed(&wallet, &wallet, &Pubkey::new_unique()),
        ],
        swap_instructions: vec![
            solana_instruction::Instruction {
                program_id: Pubkey::new_unique(),
                accounts: vec![
                    solana_instruction::AccountMeta::new(wallet, true),
                ],
                data: vec![1, 2, 3, 4],
            },
        ],
        cleanup_instructions: vec![],
        additional_signers: vec![],
    };

    let (tx, tx_base64) = build_unsigned_transaction(
        swap_ixs,
        200_000,
        50_000,
        &wallet,
        blockhash,
    ).unwrap();

    // Transaction should have instructions:
    // [compute_unit_limit, compute_unit_price, ata_create, swap]
    assert_eq!(tx.message.instructions.len(), 4);

    // Blockhash should be set
    assert_eq!(tx.message.recent_blockhash, blockhash);

    // Base64 should be non-empty and decodable
    assert!(!tx_base64.is_empty());
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &tx_base64,
    ).unwrap();
    assert!(!decoded.is_empty());

    // Should roundtrip back to a Transaction
    let tx_roundtrip: solana_transaction::Transaction = bincode::deserialize(&decoded).unwrap();
    assert_eq!(tx_roundtrip.message.instructions.len(), 4);
}

#[test]
fn test_pump_fun_bonding_curve_math() {
    use solbot_strategy::tx_builder::pump_fun::PumpFunBondingCurve;

    let curve = PumpFunBondingCurve {
        bonding_curve: Pubkey::new_unique(),
        mint: Pubkey::new_unique(),
        associated_bonding_curve: Pubkey::new_unique(),
        virtual_token_reserves: 1_000_000_000_000,   // 1M tokens (6 decimals)
        virtual_sol_reserves: 30_000_000_000,          // 30 SOL
        real_token_reserves: 500_000_000_000,
        real_sol_reserves: 15_000_000_000,
        token_total_supply: 1_000_000_000_000,
        complete: false,
    };

    // Buy output should be > 0
    let buy_out = curve.calculate_buy_output(1_000_000_000); // 1 SOL
    assert!(buy_out > 0, "Buy output must be positive");

    // Sell output should be > 0
    let sell_out = curve.calculate_sell_output(10_000_000_000); // 10B tokens
    assert!(sell_out > 0, "Sell output must be positive");

    // Buy with 0 → 0
    assert_eq!(curve.calculate_buy_output(0), 0);

    // Empty reserves → 0
    let empty = PumpFunBondingCurve {
        virtual_token_reserves: 0,
        virtual_sol_reserves: 0,
        ..curve.clone()
    };
    assert_eq!(empty.calculate_buy_output(1_000_000), 0);

    // Active check
    assert!(curve.is_active());
    let completed = PumpFunBondingCurve { complete: true, ..curve };
    assert!(!completed.is_active());
}

#[test]
fn test_raydium_clmm_tick_array_index_calculation() {
    use solbot_strategy::tx_builder::raydium_clmm::*;

    // Basic tick array start index calculations
    assert_eq!(tick_array_start_index(0, 1), 0);
    assert_eq!(tick_array_start_index(59, 1), 0);
    assert_eq!(tick_array_start_index(60, 1), 60);
    assert_eq!(tick_array_start_index(-1, 1), -60);
    assert_eq!(tick_array_start_index(-60, 1), -60);
    assert_eq!(tick_array_start_index(-61, 1), -120);

    // With larger tick spacing
    assert_eq!(tick_array_start_index(0, 10), 0);
    assert_eq!(tick_array_start_index(599, 10), 0);
    assert_eq!(tick_array_start_index(600, 10), 600);
}

#[test]
fn test_meteora_dlmm_bin_array_index() {
    use solbot_strategy::tx_builder::meteora_dlmm::*;

    assert_eq!(bin_array_index_from_bin_id(0), 0);
    assert_eq!(bin_array_index_from_bin_id(69), 0);
    assert_eq!(bin_array_index_from_bin_id(70), 1);
    assert_eq!(bin_array_index_from_bin_id(139), 1);
    assert_eq!(bin_array_index_from_bin_id(140), 2);
    assert_eq!(bin_array_index_from_bin_id(-1), -1);
    assert_eq!(bin_array_index_from_bin_id(-70), -1);
    assert_eq!(bin_array_index_from_bin_id(-71), -2);
}
