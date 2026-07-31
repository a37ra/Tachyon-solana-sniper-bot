//! Associated Token Account (ATA) management.
//!
//! Every token swap requires the wallet to have an ATA for the token mint.
//! This module handles:
//! - Looking up existing ATAs
//! - Building create-ATA instructions when needed
//! - WSOL wrapping/unwrapping for native SOL swaps

use anyhow::Result;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_sdk::system_instruction;
use spl_associated_token_account::get_associated_token_address;
use spl_associated_token_account::instruction::create_associated_token_account;

/// Native SOL mint (Wrapped SOL / WSOL).
pub const WSOL_MINT: Pubkey = spl_token::native_mint::id();

/// Derive the ATA address for a wallet + mint.
pub fn derive_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address(wallet, mint)
}

/// Build an instruction to create an ATA if it doesn't exist.
///
/// Uses `create_associated_token_account` which is idempotent-safe:
/// if the account already exists, the instruction is a no-op.
pub fn create_ata_instruction_if_needed(
    payer: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
) -> Instruction {
    create_associated_token_account(payer, wallet, mint, &spl_token::id())
}

/// Build instructions to wrap SOL into WSOL for a buy swap.
///
/// 1. Create ATA for WSOL (idempotent)
/// 2. Transfer SOL to the WSOL ATA
/// 3. SyncNative to update the WSOL balance
///
/// Returns the WSOL ATA address and the setup instructions.
pub fn wrap_sol_instructions(
    wallet: &Pubkey,
    amount_lamports: u64,
) -> Result<(Pubkey, Vec<Instruction>)> {
    let wsol_ata = derive_ata(wallet, &WSOL_MINT);

    let instructions = vec![
        // Create WSOL ATA if needed
        create_ata_instruction_if_needed(wallet, wallet, &WSOL_MINT),
        // Transfer SOL to WSOL ATA
        system_instruction::transfer(wallet, &wsol_ata, amount_lamports),
        // Sync native balance
        spl_token::instruction::sync_native(&spl_token::id(), &wsol_ata)?,
    ];

    Ok((wsol_ata, instructions))
}

/// Build instruction to close a WSOL account and reclaim rent + remaining balance.
///
/// Used after a sell swap completes (output is WSOL → unwrap to native SOL).
pub fn close_wsol_account_instruction(
    wallet: &Pubkey,
) -> Result<Instruction> {
    let wsol_ata = derive_ata(wallet, &WSOL_MINT);
    Ok(spl_token::instruction::close_account(
        &spl_token::id(),
        &wsol_ata,
        wallet,   // destination for remaining lamports
        wallet,   // authority
        &[],
    )?)
}

/// Build instructions for token output account setup (for buy swaps).
///
/// Ensures the wallet has an ATA for the token being bought.
pub fn ensure_token_ata_instructions(
    wallet: &Pubkey,
    token_mint: &Pubkey,
) -> (Pubkey, Vec<Instruction>) {
    let ata = derive_ata(wallet, token_mint);
    let instructions = vec![
        create_ata_instruction_if_needed(wallet, wallet, token_mint),
    ];
    (ata, instructions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_pubkey::Pubkey;

    #[test]
    fn test_derive_ata_deterministic() {
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata1 = derive_ata(&wallet, &mint);
        let ata2 = derive_ata(&wallet, &mint);
        assert_eq!(ata1, ata2);
    }

    #[test]
    fn test_wsol_mint_is_native() {
        assert_eq!(WSOL_MINT, spl_token::native_mint::id());
    }

    #[test]
    fn test_wrap_sol_produces_three_instructions() {
        let wallet = Pubkey::new_unique();
        let (ata, ixs) = wrap_sol_instructions(&wallet, 1_000_000).unwrap();
        assert_eq!(ixs.len(), 3);
        assert_eq!(ata, derive_ata(&wallet, &WSOL_MINT));
    }
}
