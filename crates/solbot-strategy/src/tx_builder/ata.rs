//! Associated Token Account (ATA) management.
//!
//! Supports classic SPL Token and Token-2022. Pump.fun mints are Token-2022
//! (verified 2026-08-10: all paper-reject mints owned by TokenzQdBNb…).

use anyhow::Result;
use solana_instruction::Instruction;
use solana_program::system_instruction;
use solana_pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;

/// Native SOL mint (Wrapped SOL / WSOL).
pub const WSOL_MINT: Pubkey = spl_token::native_mint::id();

/// Token-2022 program id.
pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    solana_pubkey::pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

/// Derive the ATA address for a wallet + mint (classic SPL Token program).
pub fn derive_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    derive_ata_with_program(wallet, mint, &spl_token::id())
}

/// Derive ATA for a specific token program (SPL Token or Token-2022).
pub fn derive_ata_with_program(wallet: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(wallet, mint, token_program)
}

/// Create ATA idempotent for classic SPL Token.
pub fn create_ata_instruction_if_needed(
    payer: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
) -> Instruction {
    create_ata_instruction_with_program(payer, wallet, mint, &spl_token::id())
}

/// Create ATA idempotent for the given token program.
pub fn create_ata_instruction_with_program(
    payer: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    create_associated_token_account_idempotent(payer, wallet, mint, token_program)
}

/// Build instructions to wrap SOL into WSOL for a buy swap.
pub fn wrap_sol_instructions(
    wallet: &Pubkey,
    amount_lamports: u64,
) -> Result<(Pubkey, Vec<Instruction>)> {
    let wsol_ata = derive_ata(wallet, &WSOL_MINT);

    let instructions = vec![
        create_ata_instruction_if_needed(wallet, wallet, &WSOL_MINT),
        system_instruction::transfer(wallet, &wsol_ata, amount_lamports),
        spl_token::instruction::sync_native(&spl_token::id(), &wsol_ata)?,
    ];

    Ok((wsol_ata, instructions))
}

/// Build instruction to close a WSOL account and reclaim rent + remaining balance.
pub fn close_wsol_account_instruction(wallet: &Pubkey) -> Result<Instruction> {
    let wsol_ata = derive_ata(wallet, &WSOL_MINT);
    Ok(spl_token::instruction::close_account(
        &spl_token::id(),
        &wsol_ata,
        wallet,
        wallet,
        &[],
    )?)
}

/// Ensure wallet ATA for classic SPL Token mint.
pub fn ensure_token_ata_instructions(
    wallet: &Pubkey,
    token_mint: &Pubkey,
) -> (Pubkey, Vec<Instruction>) {
    ensure_token_ata_instructions_with_program(wallet, token_mint, &spl_token::id())
}

/// Ensure wallet ATA for mint under the given token program.
pub fn ensure_token_ata_instructions_with_program(
    wallet: &Pubkey,
    token_mint: &Pubkey,
    token_program: &Pubkey,
) -> (Pubkey, Vec<Instruction>) {
    let ata = derive_ata_with_program(wallet, token_mint, token_program);
    let instructions = vec![create_ata_instruction_with_program(
        wallet,
        wallet,
        token_mint,
        token_program,
    )];
    (ata, instructions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_ata_deterministic() {
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        assert_eq!(derive_ata(&wallet, &mint), derive_ata(&wallet, &mint));
    }

    #[test]
    fn token_2022_ata_differs_from_classic() {
        let wallet = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let classic = derive_ata_with_program(&wallet, &mint, &spl_token::id());
        let t22 = derive_ata_with_program(&wallet, &mint, &TOKEN_2022_PROGRAM_ID);
        assert_ne!(classic, t22);
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
