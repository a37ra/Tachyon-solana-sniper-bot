//! Raydium CLMM (Concentrated Liquidity Market Maker) swap builder.
//!
//! Program ID: CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK
//!
//! CLMM uses tick-based concentrated liquidity (similar to Uniswap v3).
//! Swap instructions must include relevant tick array accounts for
//! price traversal.

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::str::FromStr;

pub const RAYDIUM_CLMM_PROGRAM: Pubkey =
    solana_pubkey::pubkey!("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");

/// Anchor discriminator for swap_v2
const SWAP_V2_DISCRIMINATOR: [u8; 8] = [0x2b, 0x04, 0xed, 0x0b, 0x1a, 0xc9, 0x1e, 0x62];

/// Anchor discriminator for legacy swap (V1) - global:swap
const SWAP_V1_DISCRIMINATOR: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];

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
    /// Inline bitmap tracking initialized tick-arrays (1024 bits = 16 u64)
    pub tick_array_bitmap: [u64; 16],
    /// Current tick index — used to determine which tick arrays to include.
    pub tick_current: i32,
    /// Tick spacing — determines tick array address derivation.
    pub tick_spacing: u16,
}

impl ClmmPoolState {
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.len() < 8 + 1 + 32 * 7 + 2 + 16 + 16 + 4 {
            bail!("CLMM account data too short: {} bytes", data.len());
        }

        // Official Raydium CLMM PoolState layout (from raydium-io/raydium-clmm)
        // Offset calculation based on official struct:
        // 0-8: discriminator
        // 8-9: bump (1 byte)
        // 9-41: amm_config (32 bytes)
        // 41-73: owner (32 bytes)
        // 73-105: token_mint_0 (32 bytes)
        // 105-137: token_mint_1 (32 bytes)
        // 137-169: token_vault_0 (32 bytes)
        // 169-201: token_vault_1 (32 bytes)
        // 201-233: observation_key (32 bytes)
        // 233-234: mint_decimals_0 (1 byte)
        // 234-235: mint_decimals_1 (1 byte)
        // 235-237: tick_spacing (2 bytes)
        // 237-253: liquidity (16 bytes)
        // 253-269: sqrt_price_x64 (16 bytes)
        // 269-273: tick_current (4 bytes)
        // 273-275: padding3 (2 bytes)
        // 275-277: padding4 (2 bytes)
        // 277-293: fee_growth_global_0_x64 (16 bytes)
        // 293-309: fee_growth_global_1_x64 (16 bytes)
        // 309-317: protocol_fees_token_0 (8 bytes)
        // 317-325: protocol_fees_token_1 (8 bytes)
        // 325-357: padding5 [u128; 4] = 64 bytes
        // 357-358: status (1 byte)
        // 358-359: fee_on (1 byte)
        // 359-365: padding [u8; 6] = 6 bytes
        // 365-872: reward_infos [RewardInfo; 3] = 507 bytes
        // 872-1000: tick_array_bitmap [u64; 16] = 128 bytes
        
        let base_offset = 9; // discriminator(8) + bump(1)
        
        // Calculate offsets based on official structure
        let tick_spacing_offset = base_offset + 224 + 2; // after observation_key(32*7=224) + mint_decimals(2)
        let tick_current_offset = tick_spacing_offset + 2 + 16 + 16; // after tick_spacing(2) + liquidity(16) + sqrt_price_x64(16)
        let tick_array_bitmap_offset = 872; // after reward_infos (365 + 507 = 872)
        
        // Read tick_array_bitmap (16 u64 = 128 bytes)
        let mut tick_array_bitmap = [0u64; 16];
        for i in 0..16 {
            let offset = tick_array_bitmap_offset + i * 8;
            if offset + 8 <= data.len() {
                tick_array_bitmap[i] = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap_or([0; 8]));
            }
        }
        
        Ok(Self {
            pool_id: Pubkey::default(), // pool_id is the account address itself
            amm_config: read_pubkey(data, base_offset)?,
            owner: read_pubkey(data, base_offset + 32)?,
            token_mint_0: read_pubkey(data, base_offset + 64)?,
            token_mint_1: read_pubkey(data, base_offset + 96)?,
            token_vault_0: read_pubkey(data, base_offset + 128)?,
            token_vault_1: read_pubkey(data, base_offset + 160)?,
            observation_key: read_pubkey(data, base_offset + 192)?,
            tick_array_bitmap_extension: None, // loaded separately if needed
            tick_array_bitmap,
            tick_current: i32::from_le_bytes(
                data[tick_current_offset..tick_current_offset + 4].try_into().unwrap_or([0; 4])
            ),
            tick_spacing: u16::from_le_bytes(
                data[tick_spacing_offset..tick_spacing_offset + 2].try_into().unwrap_or([0; 2])
            ),
        })
    }
}

/// Constants for tick array bitmap navigation
const TICK_ARRAY_SIZE: i32 = 60;
const TICK_ARRAY_BITMAP_SIZE: i32 = 512;
const EXTENSION_TICKARRAY_BITMAP_SIZE: usize = 14;

/// TickArrayBitmapExtension structure for pools with wide tick ranges
#[derive(Debug, Clone)]
pub struct TickArrayBitmapExtension {
    pub pool_id: Pubkey,
    pub positive_tick_array_bitmap: [[u64; 8]; EXTENSION_TICKARRAY_BITMAP_SIZE],
    pub negative_tick_array_bitmap: [[u64; 8]; EXTENSION_TICKARRAY_BITMAP_SIZE],
}

impl TickArrayBitmapExtension {
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.len() < 32 + 64 * EXTENSION_TICKARRAY_BITMAP_SIZE * 2 {
            bail!("TickArrayBitmapExtension data too short: {} bytes", data.len());
        }

        let pool_id = Pubkey::try_from(&data[0..32])?;
        
        let mut positive_tick_array_bitmap = [[0u64; 8]; EXTENSION_TICKARRAY_BITMAP_SIZE];
        let mut negative_tick_array_bitmap = [[0u64; 8]; EXTENSION_TICKARRAY_BITMAP_SIZE];
        
        let offset = 32;
        for i in 0..EXTENSION_TICKARRAY_BITMAP_SIZE {
            let bitmap_offset = offset + i * 64;
            for j in 0..8 {
                let limb_offset = bitmap_offset + j * 8;
                if limb_offset + 8 <= data.len() {
                    positive_tick_array_bitmap[i][j] = u64::from_le_bytes(
                        data[limb_offset..limb_offset + 8].try_into().unwrap_or([0; 8])
                    );
                }
            }
        }
        
        let neg_offset = offset + 64 * EXTENSION_TICKARRAY_BITMAP_SIZE;
        for i in 0..EXTENSION_TICKARRAY_BITMAP_SIZE {
            let bitmap_offset = neg_offset + i * 64;
            for j in 0..8 {
                let limb_offset = bitmap_offset + j * 8;
                if limb_offset + 8 <= data.len() {
                    negative_tick_array_bitmap[i][j] = u64::from_le_bytes(
                        data[limb_offset..limb_offset + 8].try_into().unwrap_or([0; 8])
                    );
                }
            }
        }
        
        Ok(Self {
            pool_id,
            positive_tick_array_bitmap,
            negative_tick_array_bitmap,
        })
    }
    
    /// Get bitmap offset for a given tick index in extension
    fn get_bitmap_offset(tick_index: i32, tick_spacing: u16) -> Result<usize> {
        let ticks_in_one_bitmap = i32::from(tick_spacing) * TICK_ARRAY_SIZE * TICK_ARRAY_BITMAP_SIZE;
        let mut offset = tick_index.abs() / ticks_in_one_bitmap - 1;
        if tick_index < 0 && tick_index.abs() % ticks_in_one_bitmap == 0 {
            offset -= 1;
        }
        Ok(offset as usize)
    }
    
    /// Check if tick array is initialized in extension
    pub fn check_tick_array_is_initialized(
        &self,
        tick_array_start_index: i32,
        tick_spacing: u16,
    ) -> bool {
        let offset = match Self::get_bitmap_offset(tick_array_start_index, tick_spacing) {
            Ok(o) => o,
            Err(_) => return false,
        };
        
        if offset >= EXTENSION_TICKARRAY_BITMAP_SIZE {
            return false;
        }
        
        let bitmap = if tick_array_start_index < 0 {
            self.negative_tick_array_bitmap[offset]
        } else {
            self.positive_tick_array_bitmap[offset]
        };
        
        let ticks_per_bitmap = i32::from(tick_spacing) * TICK_ARRAY_SIZE * TICK_ARRAY_BITMAP_SIZE;
        let array_offset_in_bitmap = (tick_array_start_index.abs() % ticks_per_bitmap) / (i32::from(tick_spacing) * TICK_ARRAY_SIZE);
        let bit_pos = array_offset_in_bitmap as usize;
        
        let limb_index = bit_pos / 64;
        let bit_in_limb = bit_pos % 64;
        
        if limb_index < 8 {
            (bitmap[limb_index] & (1u64 << bit_in_limb)) != 0
        } else {
            false
        }
    }
}

/// Calculate tick array start index from tick index
pub fn tick_array_start_index(tick_index: i32, tick_spacing: u16) -> i32 {
    let multiplier = i32::from(tick_spacing) * TICK_ARRAY_SIZE;
    let mut compressed = tick_index / multiplier;
    if tick_index < 0 && tick_index % multiplier != 0 {
        compressed -= 1;
    }
    compressed * multiplier
}

/// Find next initialized tick array start index from bitmap
/// Based on TypeScript SDK's scanLinearBitmap approach
/// The bitmap uses offset = -512 for the inline pool bitmap
/// A bit at position bit_pos represents tick array start index: (bit_pos + offset) * tickSpacing * TICK_ARRAY_SIZE
pub fn next_initialized_tick_array_start_index(
    tick_array_bitmap: &[u64; 16],
    last_tick_array_start_index: i32,
    tick_spacing: u16,
    zero_for_one: bool,
) -> Option<i32> {
    let ticks_per_array = i32::from(tick_spacing) * TICK_ARRAY_SIZE;
    let offset = -TICK_ARRAY_BITMAP_SIZE; // -512 for inline pool bitmap
    
    // Calculate threshold using TypeScript SDK formula: tick / (tickSpacing * TICK_ARRAY_SIZE) - offset
    // This gives us the bit position threshold
    let threshold = (last_tick_array_start_index as f64 / (ticks_per_array as f64)) - (offset as f64);
    
    let total_bits: usize = 1024; // 16 u64 * 64 bits
    
    if zero_for_one {
        // zeroForOne: tick from upper to lower - search from threshold down to 0
        // TypeScript uses Math.floor(threshold) for lte
        let end_bit = (threshold.floor() as i32).max(0).min((total_bits - 1) as i32) as usize;
        
        for bit_pos in (0..=end_bit).rev() {
            let limb_index = bit_pos / 64;
            let bit_in_limb = bit_pos % 64;
            if limb_index < 16 && (tick_array_bitmap[limb_index] & (1u64 << bit_in_limb)) != 0 {
                // Convert bit position back to tick array start index
                // Formula: (bit_pos + offset) * tickSpacing * TICK_ARRAY_SIZE
                let array_index = ((bit_pos as i32 + offset) * ticks_per_array) as i32;
                // Only return if it's different from last and in the correct direction
                if array_index < last_tick_array_start_index {
                    return Some(array_index);
                }
            }
        }
        None
    } else {
        // oneForZero: tick from lower to upper - search from threshold up to total_bits-1
        // TypeScript uses Math.ceil(threshold) for gte
        let start_bit = (threshold.ceil() as i32).max(0).min((total_bits - 1) as i32) as usize;
        
        for bit_pos in start_bit..total_bits {
            let limb_index = bit_pos / 64;
            let bit_in_limb = bit_pos % 64;
            if limb_index < 16 && (tick_array_bitmap[limb_index] & (1u64 << bit_in_limb)) != 0 {
                // Convert bit position back to tick array start index
                let array_index = ((bit_pos as i32 + offset) * ticks_per_array) as i32;
                // Only return if it's different from last and in the correct direction
                if array_index > last_tick_array_start_index {
                    return Some(array_index);
                }
            }
        }
        None
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
            &start_tick_index.to_be_bytes(),
        ],
        &RAYDIUM_CLMM_PROGRAM,
    );
    Ok(address)
}

/// Build Raydium CLMM swap instructions.
pub fn build_swap(
    params: &SwapParams,
    pool_state: &ClmmPoolState,
    pool_id: &Pubkey,
    _extension: Option<&TickArrayBitmapExtension>,
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

    // Determine input and output mints based on swap direction
    let (input_mint, output_mint) = if a_to_b {
        (pool_state.token_mint_0, pool_state.token_mint_1)
    } else {
        (pool_state.token_mint_1, pool_state.token_mint_0)
    };

    // WSOL mint address
    let wsol_mint = Pubkey::from_str("So11111111111111111111111111111111111111112")?;

    // Setup ATAs based on actual input/output mints
    let (user_input_ata, user_output_ata) = if input_mint == wsol_mint {
        let (wsol_ata, wrap_ixs) = ata::wrap_sol_instructions(wallet, params.amount_in)?;
        setup_instructions.extend(wrap_ixs);
        let (token_ata, token_ixs) = ata::ensure_token_ata_instructions(wallet, &output_mint);
        setup_instructions.extend(token_ixs);
        cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);
        (wsol_ata, token_ata)
    } else if output_mint == wsol_mint {
        let (wsol_ata, wsol_ixs) = ata::wrap_sol_instructions(wallet, 0)?;
        setup_instructions.extend(wsol_ixs);
        let (token_ata, token_ixs) = ata::ensure_token_ata_instructions(wallet, &input_mint);
        setup_instructions.extend(token_ixs);
        cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);
        (token_ata, wsol_ata)
    } else {
        // Neither input nor output is SOL - both are tokens
        let (input_ata, input_ixs) = ata::ensure_token_ata_instructions(wallet, &input_mint);
        setup_instructions.extend(input_ixs);
        let (output_ata, output_ixs) = ata::ensure_token_ata_instructions(wallet, &output_mint);
        setup_instructions.extend(output_ixs);
        (input_ata, output_ata)
    };

    let (input_vault, output_vault) = if a_to_b {
        (&pool_state.token_vault_0, &pool_state.token_vault_1)
    } else {
        (&pool_state.token_vault_1, &pool_state.token_vault_0)
    };

    // Derive tick array accounts using bitmap navigation
    let start_idx = tick_array_start_index(pool_state.tick_current, pool_state.tick_spacing);
    let mut tick_arrays = Vec::new();
    
    println!("📊 Using bitmap navigation:");
    println!("  tick_current: {}", pool_state.tick_current);
    println!("  start_idx: {}", start_idx);
    println!("  tick_spacing: {}", pool_state.tick_spacing);
    println!("  a_to_b: {}", a_to_b);
    
    // Add current tick array
    let current_tick_array_addr = derive_tick_array_address(pool_id, start_idx)?;
    tick_arrays.push(current_tick_array_addr);
    println!("  Current tick array: idx={}, addr={}", start_idx, current_tick_array_addr);
    
    // Find next initialized tick arrays using bitmap
    let mut last_idx = start_idx;
    let zero_for_one = a_to_b;
    
    for _ in 0..4 {
        if let Some(next_idx) = next_initialized_tick_array_start_index(
            &pool_state.tick_array_bitmap,
            last_idx,
            pool_state.tick_spacing,
            zero_for_one,
        ) {
            let next_addr = derive_tick_array_address(pool_id, next_idx)?;
            tick_arrays.push(next_addr);
            println!("  Found next tick array: idx={}, addr={}", next_idx, next_addr);
            last_idx = next_idx;
        } else {
            println!("  No more initialized tick arrays found in bitmap");
            break;
        }
    }
    
    if tick_arrays.is_empty() {
        bail!("No initialized tick arrays found via bitmap navigation");
    }
    
    println!("📊 Tick arrays found:");
    for (i, addr) in tick_arrays.iter().enumerate() {
        println!("  [{}]: {}", i, addr);
    }

    // sqrt_price_limit: 0 means no limit
    let sqrt_price_limit: u128 = 0;

    // Build instruction data (using legacy V1 swap for fewer accounts)
    let mut instruction_data = Vec::with_capacity(8 + 8 + 8 + 16 + 1);
    instruction_data.extend_from_slice(&SWAP_V1_DISCRIMINATOR);
    instruction_data.extend_from_slice(&params.amount_in.to_le_bytes());
    instruction_data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());
    instruction_data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
    instruction_data.push(if a_to_b { 1 } else { 0 }); // is_base_input = true for swap_base_in

    let mut accounts = vec![
        AccountMeta::new_readonly(*wallet, true),          // payer
        AccountMeta::new_readonly(pool_state.amm_config, false),
        AccountMeta::new(*pool_id, false),                 // pool state
        AccountMeta::new(user_input_ata, false),
        AccountMeta::new(user_output_ata, false),
        AccountMeta::new(*input_vault, false),
        AccountMeta::new(*output_vault, false),
        AccountMeta::new(pool_state.observation_key, false),
        AccountMeta::new_readonly(spl_token::id(), false), // token_program (V1 only needs one)
    ];
    
    // Add tick arrays
    for tick_array in tick_arrays {
        accounts.push(AccountMeta::new(tick_array, false));
    }
    
    let swap_ix = Instruction {
        program_id: RAYDIUM_CLMM_PROGRAM,
        accounts,
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
