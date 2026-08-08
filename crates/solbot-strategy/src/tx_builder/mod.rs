//! Transaction builder module — constructs unsigned swap transactions for all supported DEXes.
//!
//! Architecture:
//! - Each DEX has its own builder implementing `SwapInstructionBuilder`
//! - Common infrastructure: compute budget, blockhash cache, ATA management
//! - The builder produces a serialized unsigned `Transaction` in base64,
//!   ready to be sent to `solbot-signer` for signing via IPC

pub mod ata;
pub mod blockhash_cache;
pub mod compute_budget;
pub mod meteora_dlmm;
pub mod meteora_dynamic;
pub mod pump_fun;
pub mod raydium_amm_v4;
pub mod raydium_clmm;
pub mod raydium_cpmm;

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;

/// Direction of a swap from the bot's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    /// Buy token: SOL → Token
    Buy,
    /// Sell token: Token → SOL
    Sell,
}

/// Parameters for building a swap transaction.
#[derive(Clone)]
pub struct SwapParams {
    /// The token mint address being traded.
    pub mint: Pubkey,
    /// The pool / AMM account address.
    pub pool_address: Pubkey,
    /// The DEX program ID.
    pub dex_program_id: Pubkey,
    /// The wallet (payer / authority) pubkey.
    pub wallet: Pubkey,
    /// Buy or Sell direction.
    pub direction: SwapDirection,
    /// Amount in lamports (for Buy: SOL amount; for Sell: token amount in smallest unit).
    pub amount_in: u64,
    /// Minimum amount out (slippage-adjusted).
    pub minimum_amount_out: u64,
    /// Slippage tolerance in basis points.
    pub slippage_bps: u32,
}

/// Result of building swap instructions for a specific DEX.
#[derive(Debug, Clone)]
pub struct SwapInstructions {
    /// Pre-swap instructions (e.g., create ATA, wrap SOL).
    pub setup_instructions: Vec<Instruction>,
    /// The actual swap instruction(s).
    pub swap_instructions: Vec<Instruction>,
    /// Post-swap instructions (e.g., unwrap SOL, close accounts).
    pub cleanup_instructions: Vec<Instruction>,
    /// Accounts that need to sign (besides the wallet).
    pub additional_signers: Vec<Pubkey>,
}

/// Trait implemented by each DEX-specific swap builder.
pub trait SwapInstructionBuilder: Send + Sync {
    /// Build the swap instructions for the given parameters.
    ///
    /// The caller is responsible for:
    /// - Adding compute budget instructions
    /// - Setting the recent blockhash
    /// - Signing the transaction
    fn build_swap_instructions(
        &self,
        params: &SwapParams,
        pool_state: &[u8],
    ) -> Result<SwapInstructions>;

    /// The DEX program ID this builder handles.
    fn program_id(&self) -> Pubkey;

    /// Human-readable name for logging.
    fn name(&self) -> &'static str;
}

/// Build a complete unsigned transaction from swap parameters.
///
/// This is the main entry point used by the actor trading loop:
/// 1. Look up / create ATA
/// 2. Build DEX-specific swap instructions  
/// 3. Add compute budget
/// 4. Assemble into unsigned Transaction
/// 5. Serialize to base64 for signer IPC
pub fn compute_priority_fee_micro_lamports(baseline_micro_lamports: u64, multiplier: f64) -> u64 {
    (baseline_micro_lamports as f64 * multiplier).round() as u64
}

pub fn build_unsigned_transaction(
    swap_instructions: SwapInstructions,
    compute_units: u32,
    priority_fee_micro_lamports: u64,
    wallet: &Pubkey,
    recent_blockhash: solana_sdk::hash::Hash,
) -> Result<(Transaction, String)> {
    let mut all_instructions = Vec::new();

    // 1. Compute budget instructions first
    all_instructions.extend(compute_budget::create_compute_budget_instructions(
        compute_units,
        priority_fee_micro_lamports,
    ));

    // 2. Setup instructions (ATA creation, SOL wrapping, etc.)
    all_instructions.extend(swap_instructions.setup_instructions);

    // 3. Swap instructions
    all_instructions.extend(swap_instructions.swap_instructions);

    // 4. Cleanup instructions (unwrap SOL, close accounts, etc.)
    all_instructions.extend(swap_instructions.cleanup_instructions);

    // Build unsigned transaction
    let tx = Transaction::new_with_payer(&all_instructions, Some(wallet));

    // We need to set the blockhash but NOT sign (signer does that)
    let mut tx = tx;
    tx.message.recent_blockhash = recent_blockhash;

    // Serialize to base64 for IPC
    let tx_bytes = bincode::serialize(&tx)?;
    let tx_base64 = STANDARD.encode(&tx_bytes);

    Ok((tx, tx_base64))
}

/// Calculate minimum amount out given slippage tolerance.
///
/// `expected_out` is the amount we expect from the swap.
/// `slippage_bps` is the tolerance (e.g., 500 = 5%).
pub fn apply_slippage(expected_out: u64, slippage_bps: u32) -> u64 {
    let factor = 10_000u128.saturating_sub(slippage_bps as u128);
    ((expected_out as u128) * factor / 10_000u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_slippage() {
        // 5% slippage on 1000
        assert_eq!(apply_slippage(1000, 500), 950);
        // 1% slippage on 10000
        assert_eq!(apply_slippage(10000, 100), 9900);
        // 0% slippage
        assert_eq!(apply_slippage(1000, 0), 1000);
        // 100% slippage
        assert_eq!(apply_slippage(1000, 10000), 0);
        // overflow protection
        assert_eq!(
            apply_slippage(u64::MAX, 500),
            (u64::MAX as u128 * 9500 / 10000) as u64
        );
    }

    #[test]
    fn test_compute_priority_fee_micro_lamports() {
        assert_eq!(compute_priority_fee_micro_lamports(1000, 1.0), 1000);
        assert_eq!(compute_priority_fee_micro_lamports(1000, 2.0), 2000);
        assert_eq!(compute_priority_fee_micro_lamports(1000, 3.0), 3000);
    }

    #[test]
    fn test_build_unsigned_transaction_includes_stage_specific_priority_fee() {
        use solana_sdk::compute_budget::{id as compute_budget_program_id, ComputeBudgetInstruction};
        use solana_sdk::hash::Hash;
        use solana_sdk::system_program;

        let wallet = Pubkey::new_unique();
        let dummy_ix = Instruction::new_with_bincode(system_program::id(), &0u8, vec![]);
        let swap_instructions = SwapInstructions {
            setup_instructions: vec![],
            swap_instructions: vec![dummy_ix],
            cleanup_instructions: vec![],
            additional_signers: vec![],
        };

        let tx_stage1 = build_unsigned_transaction(
            swap_instructions.clone(),
            200_000,
            compute_priority_fee_micro_lamports(1000, 2.0),
            &wallet,
            Hash::new_unique(),
        )
        .unwrap()
        .0;

        let tx_stage2 = build_unsigned_transaction(
            swap_instructions,
            200_000,
            compute_priority_fee_micro_lamports(1000, 3.0),
            &wallet,
            Hash::new_unique(),
        )
        .unwrap()
        .0;

        let price_ix_stage1 = &tx_stage1.message.instructions[1];
        let price_ix_stage2 = &tx_stage2.message.instructions[1];

        assert_eq!(
            tx_stage1.message.account_keys[price_ix_stage1.program_id_index as usize],
            compute_budget_program_id(),
        );
        assert_eq!(
            tx_stage2.message.account_keys[price_ix_stage2.program_id_index as usize],
            compute_budget_program_id(),
        );

        let expected_stage1 = ComputeBudgetInstruction::set_compute_unit_price(2000);
        let expected_stage2 = ComputeBudgetInstruction::set_compute_unit_price(3000);

        assert_eq!(price_ix_stage1.data, expected_stage1.data);
        assert_eq!(price_ix_stage2.data, expected_stage2.data);
        assert_ne!(price_ix_stage1.data, price_ix_stage2.data);
    }
}
