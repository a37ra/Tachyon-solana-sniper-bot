//! Raydium CLMM (Concentrated Liquidity Market Maker) swap builder.
//!
//! Program ID: CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK
//!
//! CLMM uses tick-based concentrated liquidity (similar to Uniswap v3).
//! Swap instructions must include relevant tick array accounts for
//! price traversal.

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

pub const RAYDIUM_CLMM_PROGRAM: Pubkey =
    solana_sdk::pubkey!("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");

/// Anchor discriminator for swap_v2
const SWAP_V2_DISCRIMINATOR: [u8; 8] = [0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62];

/// CLMM pool state parsed from on-chain data.
#[derive(Debug, Clone)]
pub struct ClmmPoolState {
    pub pool_id: Pubkey,
    pub amm_config: Pubkey,
    pub owner: Pubkey,
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
    pub observation_key: Pubkey,
    pub tick_array_bitmap_extension: Option<Pubkey>,
    /// Current tick index — used to determine which tick arrays to include.
    pub tick_current: i32,
    /// Tick spacing — determines tick array address derivation.
    pub tick_spacing: u16,
}

impl ClmmPoolState {
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.len() < 8 + 32 * 8 + 8 {
            bail!("CLMM account data too short: {} bytes", data.len());
        }

        let offset = 8; // anchor discriminator
        Ok(Self {
            pool_id: Pubkey::default(), // pool_id is the account address itself
            amm_config: read_pubkey(data, offset)?,
            owner: read_pubkey(data, offset + 32)?,
            token_mint_0: read_pubkey(data, offset + 64)?,
            token_mint_1: read_pubkey(data, offset + 96)?,
            token_vault_0: read_pubkey(data, offset + 128)?,
            token_vault_1: read_pubkey(data, offset + 160)?,
            observation_key: read_pubkey(data, offset + 192)?,
            tick_array_bitmap_extension: None, // loaded separately if needed
            tick_current: i32::from_le_bytes(
                data[offset + 224 + 33..offset + 224 + 37]
                    .try_into()
                    .unwrap_or([0; 4]),
            ),
            tick_spacing: u16::from_le_bytes(
                data[offset + 224 + 37..offset + 224 + 39]
                    .try_into()
                    .unwrap_or([0; 2]),
            ),
        })
    }
}

/// Derive tick array address for a given start tick index.
pub fn derive_tick_array_address(
    pool_id: &Pubkey,
    start_tick_index: i32,
) -> Result<Pubkey> {
    let (address, _bump) = Pubkey::find_program_address(
        &[
            b"tick_array",
            pool_id.as_ref(),
            &start_tick_index.to_le_bytes(),
        ],
        &RAYDIUM_CLMM_PROGRAM,
    );
    Ok(address)
}

/// Calculate the start tick index for a tick array containing the given tick.
pub fn tick_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
    let ticks_per_array = tick_spacing as i32 * 60; // standard: 60 ticks per array
    if ticks_per_array == 0 {
        return 0;
    }
    let mut start = tick_index / ticks_per_array * ticks_per_array;
    if tick_index < 0 && tick_index % ticks_per_array != 0 {
        start -= ticks_per_array;
    }
    start
}

/// Build Raydium CLMM swap instructions.
pub fn build_swap(
    params: &SwapParams,
    pool_state: &ClmmPoolState,
    pool_id: &Pubkey,
) -> Result<SwapInstructions> {
    let wallet = &params.wallet;
    let mut setup_instructions = Vec::new();
    let mut cleanup_instructions = Vec::new();

    let is_mint_token0 = params.mint == pool_state.token_mint_0;

    // Determine swap direction in pool terms (a_to_b or b_to_a)
    let a_to_b = match params.direction {
        SwapDirection::Buy => !is_mint_token0, // buying token0 means b→a, buying token1 means a→b
        SwapDirection::Sell => is_mint_token0,   // selling token0 means a→b
    };

    // Setup ATAs
    let (user_input_ata, user_output_ata) = match params.direction {
        SwapDirection::Buy => {
            let (wsol_ata, wrap_ixs) = ata::wrap_sol_instructions(wallet, params.amount_in)?;
            setup_instructions.extend(wrap_ixs);
            let (token_ata, token_ixs) = ata::ensure_token_ata_instructions(wallet, &params.mint);
            setup_instructions.extend(token_ixs);
            cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);
            (wsol_ata, token_ata)
        }
        SwapDirection::Sell => {
            let token_ata = ata::derive_ata(wallet, &params.mint);
            let (wsol_ata, wsol_ixs) = ata::wrap_sol_instructions(wallet, 0)?;
            setup_instructions.extend(wsol_ixs);
            cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);
            (token_ata, wsol_ata)
        }
    };

    let (input_vault, output_vault) = if a_to_b {
        (&pool_state.token_vault_0, &pool_state.token_vault_1)
    } else {
        (&pool_state.token_vault_1, &pool_state.token_vault_0)
    };

    // Derive tick array accounts (need 3 for safe traversal)
    let start_idx = tick_array_start_index(pool_state.tick_current, pool_state.tick_spacing);
    let ticks_per_array = pool_state.tick_spacing as i32 * 60;
    let direction_mult = if a_to_b { -1 } else { 1 };

    let tick_array_0 = derive_tick_array_address(pool_id, start_idx)?;
    let tick_array_1 = derive_tick_array_address(pool_id, start_idx + direction_mult * ticks_per_array)?;
    let tick_array_2 = derive_tick_array_address(pool_id, start_idx + direction_mult * ticks_per_array * 2)?;

    // sqrt_price_limit: 0 means no limit
    let sqrt_price_limit: u128 = 0;

    // Build instruction data
    let mut instruction_data = Vec::with_capacity(8 + 8 + 8 + 16 + 1);
    instruction_data.extend_from_slice(&SWAP_V2_DISCRIMINATOR);
    instruction_data.extend_from_slice(&params.amount_in.to_le_bytes());
    instruction_data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());
    instruction_data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
    instruction_data.push(if a_to_b { 1 } else { 0 }); // is_base_input = true for swap_base_in

    let swap_ix = Instruction {
        program_id: RAYDIUM_CLMM_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(*wallet, true),          // payer
            AccountMeta::new_readonly(pool_state.amm_config, false),
            AccountMeta::new(*pool_id, false),                 // pool state
            AccountMeta::new(user_input_ata, false),
            AccountMeta::new(user_output_ata, false),
            AccountMeta::new(*input_vault, false),
            AccountMeta::new(*output_vault, false),
            AccountMeta::new(pool_state.observation_key, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(spl_token::id(), false), // token_program_2022 (can be same)
            AccountMeta::new(tick_array_0, false),
            AccountMeta::new(tick_array_1, false),
            AccountMeta::new(tick_array_2, false),
        ],
        data: instruction_data,
    };

    Ok(SwapInstructions {
        setup_instructions,
        swap_instructions: vec![swap_ix],
        cleanup_instructions,
        additional_signers: vec![],
    })
}

fn read_pubkey(data: &[u8], offset: usize) -> Result<Pubkey> {
    if offset + 32 > data.len() {
        bail!("Cannot read pubkey at offset {offset}");
    }
    let bytes: [u8; 32] = data[offset..offset + 32]
        .try_into()
        .map_err(|_| anyhow!("Invalid pubkey at offset {offset}"))?;
    Ok(Pubkey::new_from_array(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_array_start_index() {
        // tick_spacing=1, ticks_per_array=60
        assert_eq!(tick_array_start_index(30, 1), 0);
        assert_eq!(tick_array_start_index(65, 1), 60);
        assert_eq!(tick_array_start_index(-1, 1), -60);
        assert_eq!(tick_array_start_index(-60, 1), -60);
        assert_eq!(tick_array_start_index(-61, 1), -120);
    }
}
