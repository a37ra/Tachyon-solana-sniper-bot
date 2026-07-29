//! Compute Budget instructions for transaction prioritization.
//!
//! Every swap transaction needs:
//! 1. `SetComputeUnitLimit` — caps the compute units consumed
//! 2. `SetComputeUnitPrice` — sets the priority fee per compute unit
//!
//! These MUST be the first two instructions in the transaction
//! for Solana's fee prioritization to work correctly.

use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
};

/// Default compute units for a typical AMM swap.
/// Raydium AMM v4 ~150k, CLMM ~300k, Meteora DLMM ~250k.
/// We set a generous default and let the caller override.
pub const DEFAULT_COMPUTE_UNITS: u32 = 400_000;

/// Create compute budget instructions.
///
/// # Arguments
/// - `compute_units` — maximum compute units the transaction can consume
/// - `priority_fee_micro_lamports` — fee per compute unit in micro-lamports
///
/// # Returns
/// A Vec of 2 instructions: [SetComputeUnitLimit, SetComputeUnitPrice]
pub fn create_compute_budget_instructions(
    compute_units: u32,
    priority_fee_micro_lamports: u64,
) -> Vec<Instruction> {
    vec![
        ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
        ComputeBudgetInstruction::set_compute_unit_price(priority_fee_micro_lamports),
    ]
}

/// Estimate compute units needed for a DEX swap.
///
/// These are conservative estimates based on observed on-chain usage.
/// The actual CU consumption depends on pool state (tick crossings for CLMM, etc).
pub fn estimate_compute_units(dex_program_id: &str) -> u32 {
    match dex_program_id {
        // Raydium AMM v4 — simple constant-product swap
        "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" => 200_000,
        // Raydium CP-MM — similar to AMM v4
        "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" => 200_000,
        // Raydium CLMM — tick array traversal can be expensive
        "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" => 350_000,
        // Meteora DLMM — bin array traversal
        "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp" => 300_000,
        // Meteora Dynamic AMM
        "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB" => 250_000,
        // Pump.fun bonding curve
        "6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2" => 200_000,
        // Unknown — generous default
        _ => DEFAULT_COMPUTE_UNITS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_creates_two_instructions() {
        let ixs = create_compute_budget_instructions(200_000, 50_000);
        assert_eq!(ixs.len(), 2);
    }

    #[test]
    fn test_estimate_known_dex() {
        assert_eq!(
            estimate_compute_units("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"),
            200_000
        );
    }

    #[test]
    fn test_estimate_unknown_dex() {
        assert_eq!(estimate_compute_units("unknown"), DEFAULT_COMPUTE_UNITS);
    }
}
