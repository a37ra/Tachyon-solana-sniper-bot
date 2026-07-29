//! Meteora DLMM (Discrete Liquidity Market Maker) swap builder.
//!
//! Program ID: LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp
//!
//! DLMM uses bin-based concentrated liquidity. Swaps traverse bin arrays
//! based on the current active bin. Fees are variable and read dynamically.

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

pub const METEORA_DLMM_PROGRAM: Pubkey =
    solana_sdk::pubkey!("LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp");

/// Anchor discriminator for `swap` instruction.
const SWAP_DISCRIMINATOR: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];

/// DLMM pool (LB Pair) state parsed from on-chain data.
#[derive(Debug, Clone)]
pub struct DlmmPoolState {
    pub lb_pair: Pubkey,
    pub bin_array_bitmap_extension: Option<Pubkey>,
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub oracle: Pubkey,
    pub active_id: i32,
}

impl DlmmPoolState {
    pub fn from_account_data(data: &[u8], lb_pair_address: &Pubkey) -> Result<Self> {
        if data.len() < 8 + 32 * 6 + 4 {
            bail!("DLMM account data too short: {} bytes", data.len());
        }

        let offset = 8; // anchor discriminator
        Ok(Self {
            lb_pair: *lb_pair_address,
            bin_array_bitmap_extension: None,
            token_x_mint: read_pubkey(data, offset + 32 * 2)?,
            token_y_mint: read_pubkey(data, offset + 32 * 3)?,
            reserve_x: read_pubkey(data, offset + 32 * 4)?,
            reserve_y: read_pubkey(data, offset + 32 * 5)?,
            oracle: read_pubkey(data, offset + 32 * 6)?,
            active_id: i32::from_le_bytes(
                data.get(offset + 32 * 7..offset + 32 * 7 + 4)
                    .and_then(|s| s.try_into().ok())
                    .unwrap_or([0; 4]),
            ),
        })
    }
}

/// Derive bin array address for a given index.
pub fn derive_bin_array_address(lb_pair: &Pubkey, index: i64) -> Result<Pubkey> {
    let (address, _) = Pubkey::find_program_address(
        &[b"bin_array", lb_pair.as_ref(), &index.to_le_bytes()],
        &METEORA_DLMM_PROGRAM,
    );
    Ok(address)
}

/// Calculate bin array index from active bin ID.
pub fn bin_array_index_from_bin_id(bin_id: i32) -> i64 {
    let bins_per_array: i64 = 70; // Meteora uses 70 bins per array
    if bin_id >= 0 {
        bin_id as i64 / bins_per_array
    } else {
        (bin_id as i64 - bins_per_array + 1) / bins_per_array
    }
}

/// Build Meteora DLMM swap instructions.
pub fn build_swap(
    params: &SwapParams,
    pool_state: &DlmmPoolState,
) -> Result<SwapInstructions> {
    let wallet = &params.wallet;
    let mut setup_instructions = Vec::new();
    let mut cleanup_instructions = Vec::new();

    let is_mint_x = params.mint == pool_state.token_x_mint;

    // Determine swap direction: x_to_y or y_to_x
    let swap_for_y = match params.direction {
        SwapDirection::Buy => !is_mint_x,  // buying token_x means y→x, buying token_y means x→y
        SwapDirection::Sell => is_mint_x,   // selling token_x means x→y
    };

    // Setup ATAs
    let (user_source_ata, user_dest_ata) = match params.direction {
        SwapDirection::Buy => {
            let (wsol_ata, wrap_ixs) = ata::wrap_sol_instructions(wallet, params.amount_in)?;
            setup_instructions.extend(wrap_ixs);
            let (token_ata, token_ixs) = ata::ensure_token_ata_instructions(wallet, &params.mint);
            setup_instructions.extend(token_ixs);
            cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);
            if swap_for_y { (wsol_ata, token_ata) } else { (wsol_ata, token_ata) }
        }
        SwapDirection::Sell => {
            let token_ata = ata::derive_ata(wallet, &params.mint);
            let (wsol_ata, wsol_ixs) = ata::wrap_sol_instructions(wallet, 0)?;
            setup_instructions.extend(wsol_ixs);
            cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);
            (token_ata, wsol_ata)
        }
    };

    // Derive bin array accounts (current + adjacent for traversal)
    let current_bin_array_idx = bin_array_index_from_bin_id(pool_state.active_id);
    let direction_offset: i64 = if swap_for_y { -1 } else { 1 };

    let bin_array_0 = derive_bin_array_address(&pool_state.lb_pair, current_bin_array_idx)?;
    let bin_array_1 = derive_bin_array_address(
        &pool_state.lb_pair,
        current_bin_array_idx + direction_offset,
    )?;
    let bin_array_2 = derive_bin_array_address(
        &pool_state.lb_pair,
        current_bin_array_idx + direction_offset * 2,
    )?;

    // Build instruction data
    let mut instruction_data = Vec::with_capacity(8 + 8 + 8);
    instruction_data.extend_from_slice(&SWAP_DISCRIMINATOR);
    instruction_data.extend_from_slice(&params.amount_in.to_le_bytes());
    instruction_data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());

    let swap_ix = Instruction {
        program_id: METEORA_DLMM_PROGRAM,
        accounts: vec![
            AccountMeta::new(pool_state.lb_pair, false),
            AccountMeta::new_readonly(
                pool_state.bin_array_bitmap_extension.unwrap_or(METEORA_DLMM_PROGRAM),
                false,
            ),
            AccountMeta::new(pool_state.reserve_x, false),
            AccountMeta::new(pool_state.reserve_y, false),
            AccountMeta::new(user_source_ata, false),
            AccountMeta::new(user_dest_ata, false),
            AccountMeta::new_readonly(pool_state.token_x_mint, false),
            AccountMeta::new_readonly(pool_state.token_y_mint, false),
            AccountMeta::new(pool_state.oracle, false),
            AccountMeta::new_readonly(*wallet, true), // user / signer
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(system_program::id(), false),
            // Remaining accounts: bin arrays
            AccountMeta::new(bin_array_0, false),
            AccountMeta::new(bin_array_1, false),
            AccountMeta::new(bin_array_2, false),
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
    fn test_bin_array_index() {
        assert_eq!(bin_array_index_from_bin_id(0), 0);
        assert_eq!(bin_array_index_from_bin_id(69), 0);
        assert_eq!(bin_array_index_from_bin_id(70), 1);
        assert_eq!(bin_array_index_from_bin_id(-1), -1);
        assert_eq!(bin_array_index_from_bin_id(-70), -1);
        assert_eq!(bin_array_index_from_bin_id(-71), -2);
    }
}
