//! Raydium AMM v4 swap instruction builder.
//!
//! Raydium AMM v4 is a constant-product AMM (x*y=k).
//! Program ID: 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8
//!
//! The swap instruction requires the following accounts layout:
//! 0. `[]` Token program
//! 1. `[writable]` AMM (pool state)
//! 2. `[]` AMM authority (PDA)
//! 3. `[writable]` AMM open orders
//! 4. `[writable]` AMM target orders (can be same as pool in some variants)
//! 5. `[writable]` Pool coin token account
//! 6. `[writable]` Pool pc token account
//! 7. `[]` Serum DEX program
//! 8. `[writable]` Serum market
//! 9. `[writable]` Serum bids
//! 10. `[writable]` Serum asks
//! 11. `[writable]` Serum event queue
//! 12. `[writable]` Serum coin vault
//! 13. `[writable]` Serum pc vault
//! 14. `[]` Serum vault signer
//! 15. `[writable]` User source token account
//! 16. `[writable]` User destination token account
//! 17. `[signer]` User owner (wallet)

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Context, Result};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use std::str::FromStr;

/// Raydium AMM v4 program ID.
pub const RAYDIUM_AMM_V4_PROGRAM: Pubkey = solana_sdk::pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");

/// Raydium AMM authority PDA seed.
const AMM_AUTHORITY_SEED: &[u8] = b"amm authority";

/// Swap instruction discriminator for Raydium AMM v4 (instruction index 9).
const SWAP_INSTRUCTION: u8 = 9;

/// On-chain pool state — key accounts extracted from the AMM account data.
///
/// Layout offsets are stable for Raydium AMM v4.
/// Reference: https://github.com/raydium-io/raydium-amm
#[derive(Debug, Clone)]
pub struct RaydiumAmmV4PoolState {
    pub amm_id: Pubkey,
    pub amm_authority: Pubkey,
    pub amm_open_orders: Pubkey,
    pub amm_target_orders: Pubkey,
    pub pool_coin_token_account: Pubkey,
    pub pool_pc_token_account: Pubkey,
    pub serum_program_id: Pubkey,
    pub serum_market: Pubkey,
    pub serum_bids: Pubkey,
    pub serum_asks: Pubkey,
    pub serum_event_queue: Pubkey,
    pub serum_coin_vault: Pubkey,
    pub serum_pc_vault: Pubkey,
    pub serum_vault_signer: Pubkey,
    pub coin_mint: Pubkey,
    pub pc_mint: Pubkey,
}

impl RaydiumAmmV4PoolState {
    /// Parse the pool state from on-chain AMM account data.
    ///
    /// The Raydium AMM v4 account layout has Pubkeys at known offsets.
    /// Total account size: 752 bytes.
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.len() < 752 {
            bail!(
                "Raydium AMM v4 account data too short: {} bytes (expected ≥752)",
                data.len()
            );
        }

        Ok(Self {
            // Status (8 bytes) + nonce (8) + max_order (8) + depth (8) + ...
            // Pubkeys start at various offsets in the AMM layout
            amm_id: read_pubkey(data, 0 + 8)?,       // offset after status
            amm_authority: Pubkey::default(),          // derived PDA
            amm_open_orders: read_pubkey(data, 72)?,
            amm_target_orders: read_pubkey(data, 104)?,
            pool_coin_token_account: read_pubkey(data, 136)?,
            pool_pc_token_account: read_pubkey(data, 168)?,
            serum_program_id: read_pubkey(data, 432)?,
            serum_market: read_pubkey(data, 464)?,
            serum_bids: read_pubkey(data, 496)?,
            serum_asks: read_pubkey(data, 528)?,
            serum_event_queue: read_pubkey(data, 560)?,
            serum_coin_vault: read_pubkey(data, 592)?,
            serum_pc_vault: read_pubkey(data, 624)?,
            serum_vault_signer: read_pubkey(data, 656)?,
            coin_mint: read_pubkey(data, 400)?,
            pc_mint: read_pubkey(data, 368)?,
        })
    }

    /// Derive the AMM authority PDA.
    pub fn derive_authority() -> Result<Pubkey> {
        let (authority, _bump) = Pubkey::find_program_address(
            &[AMM_AUTHORITY_SEED],
            &RAYDIUM_AMM_V4_PROGRAM,
        );
        Ok(authority)
    }
}

/// Build Raydium AMM v4 swap instructions.
pub fn build_swap(
    params: &SwapParams,
    pool_state: &RaydiumAmmV4PoolState,
) -> Result<SwapInstructions> {
    let wallet = &params.wallet;
    let mut setup_instructions = Vec::new();
    let mut cleanup_instructions = Vec::new();

    // Determine source and destination based on direction
    let (user_source_ata, user_dest_ata) = match params.direction {
        SwapDirection::Buy => {
            // SOL → Token: source = WSOL ATA, destination = Token ATA
            let (wsol_ata, wrap_ixs) = ata::wrap_sol_instructions(wallet, params.amount_in)?;
            setup_instructions.extend(wrap_ixs);

            let (token_ata, token_ixs) = ata::ensure_token_ata_instructions(wallet, &params.mint);
            setup_instructions.extend(token_ixs);

            // Close WSOL account after swap to reclaim rent
            cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);

            (wsol_ata, token_ata)
        }
        SwapDirection::Sell => {
            // Token → SOL: source = Token ATA, destination = WSOL ATA
            let token_ata = ata::derive_ata(wallet, &params.mint);

            let (wsol_ata, wsol_ixs) = ata::wrap_sol_instructions(wallet, 0)?;
            setup_instructions.extend(wsol_ixs);

            // Close WSOL account after swap to get SOL back
            cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);

            (token_ata, wsol_ata)
        }
    };

    let amm_authority = RaydiumAmmV4PoolState::derive_authority()?;

    // Build the swap instruction data
    let mut instruction_data = Vec::with_capacity(17);
    instruction_data.push(SWAP_INSTRUCTION);
    instruction_data.extend_from_slice(&params.amount_in.to_le_bytes());
    instruction_data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());

    let swap_instruction = Instruction {
        program_id: RAYDIUM_AMM_V4_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new(pool_state.amm_id, false),
            AccountMeta::new_readonly(amm_authority, false),
            AccountMeta::new(pool_state.amm_open_orders, false),
            AccountMeta::new(pool_state.amm_target_orders, false),
            AccountMeta::new(pool_state.pool_coin_token_account, false),
            AccountMeta::new(pool_state.pool_pc_token_account, false),
            AccountMeta::new_readonly(pool_state.serum_program_id, false),
            AccountMeta::new(pool_state.serum_market, false),
            AccountMeta::new(pool_state.serum_bids, false),
            AccountMeta::new(pool_state.serum_asks, false),
            AccountMeta::new(pool_state.serum_event_queue, false),
            AccountMeta::new(pool_state.serum_coin_vault, false),
            AccountMeta::new(pool_state.serum_pc_vault, false),
            AccountMeta::new_readonly(pool_state.serum_vault_signer, false),
            AccountMeta::new(user_source_ata, false),
            AccountMeta::new(user_dest_ata, false),
            AccountMeta::new_readonly(*wallet, true), // signer
        ],
        data: instruction_data,
    };

    Ok(SwapInstructions {
        setup_instructions,
        swap_instructions: vec![swap_instruction],
        cleanup_instructions,
        additional_signers: vec![],
    })
}

/// Read a Pubkey from a byte slice at a given offset.
fn read_pubkey(data: &[u8], offset: usize) -> Result<Pubkey> {
    if offset + 32 > data.len() {
        bail!("Cannot read pubkey at offset {offset}: data too short");
    }
    let bytes: [u8; 32] = data[offset..offset + 32]
        .try_into()
        .map_err(|_| anyhow!("Invalid pubkey bytes at offset {offset}"))?;
    Ok(Pubkey::new_from_array(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_authority() {
        let authority = RaydiumAmmV4PoolState::derive_authority().unwrap();
        // Should be a valid pubkey (not all zeros)
        assert_ne!(authority, Pubkey::default());
    }

    #[test]
    fn test_swap_instruction_data_layout() {
        let mut data = Vec::new();
        data.push(SWAP_INSTRUCTION);
        data.extend_from_slice(&1000u64.to_le_bytes());
        data.extend_from_slice(&900u64.to_le_bytes());
        assert_eq!(data.len(), 17);
        assert_eq!(data[0], 9); // instruction discriminator
    }
}
