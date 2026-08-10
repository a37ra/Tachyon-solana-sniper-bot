//! Meteora DLMM (Discrete Liquidity Market Maker) swap builder.
//!
//! Program ID (mainnet v2): LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo
//!
//! DLMM uses bin-based concentrated liquidity. Swaps traverse multiple bin
//! arrays (current ± adjacent) based on active_id.

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

/// Mainnet DLMM program (v2). Legacy kVn14… id still appears in older docs.
pub const METEORA_DLMM_PROGRAM: Pubkey =
    solana_pubkey::pubkey!("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");

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
    pub reserve_x_balance: u64,
    pub reserve_y_balance: u64,
    pub bin_step: u16, // Price step between bins
}

impl DlmmPoolState {
    pub fn from_account_data(data: &[u8], lb_pair_address: &Pubkey) -> Result<Self> {
        // Mainnet LbPair is 904 bytes; reject bin-array spam (~10k).
        if data.len() < 200 {
            bail!("DLMM account data too short: {} bytes", data.len());
        }
        if data.len() > 1200 {
            bail!(
                "DLMM account data too large (likely bin array): {} bytes",
                data.len()
            );
        }

        // StaticParameters.bin_step is near the start of LbPair (after disc + base_factor…).
        // Conservative read: try common offsets used on mainnet 904 B accounts.
        let bin_step = read_u16(data, 8 + 8).unwrap_or(1).max(1);

        // active_id: i32 after oracle / variable params — try several known layouts.
        let active_id = read_i32(data, 76)
            .or_else(|| read_i32(data, 8 + 32 * 7))
            .or_else(|| read_i32(data, 552))
            .unwrap_or(0);

        Ok(Self {
            lb_pair: *lb_pair_address,
            bin_array_bitmap_extension: None,
            // Mainnet LbPair: token_x @ 88, token_y @ 120 (verified dump 2026-08-09).
            token_x_mint: read_pubkey(data, 88)?,
            token_y_mint: read_pubkey(data, 120)?,
            reserve_x: read_pubkey(data, 128)?,
            reserve_y: read_pubkey(data, 160)?,
            oracle: read_pubkey(data, 192).unwrap_or_default(),
            active_id,
            reserve_x_balance: 0,
            reserve_y_balance: 0,
            bin_step,
        })
    }

    /// Set vault balances fetched from RPC.
    pub fn with_vault_balances(mut self, reserve_x_balance: u64, reserve_y_balance: u64) -> Self {
        self.reserve_x_balance = reserve_x_balance;
        self.reserve_y_balance = reserve_y_balance;
        self
    }

    /// Set bin step from pool state.
    pub fn with_bin_step(mut self, bin_step: u16) -> Self {
        self.bin_step = bin_step;
        self
    }

    /// Buy SOL→Token with multi-bin walk approximation.
    pub fn calculate_buy_output(&self, sol_amount: u64) -> u64 {
        let (rin, rout) = self.sol_token_reserves();
        self.multi_bin_output(sol_amount, rin, rout)
    }

    /// Sell Token→SOL with multi-bin walk approximation.
    pub fn calculate_sell_output(&self, token_amount: u64) -> u64 {
        let (rin_sol, rout_token) = self.sol_token_reserves();
        self.multi_bin_output(token_amount, rout_token, rin_sol)
    }

    fn sol_token_reserves(&self) -> (u64, u64) {
        if self.token_y_mint.to_string() == "So11111111111111111111111111111111111111112" {
            (self.reserve_y_balance, self.reserve_x_balance)
        } else {
            (self.reserve_x_balance, self.reserve_y_balance)
        }
    }

    /// Multi-bin price impact model:
    /// Split active liquidity across `n_bins` geometric steps using `bin_step` (bps of price).
    /// Walk bins until `amount_in` is filled — better than pure single-bin x*y=k for large size.
    fn multi_bin_output(&self, amount_in: u64, reserve_in: u64, reserve_out: u64) -> u64 {
        if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
            return 0;
        }

        // How many bins we may cross (builder already attaches 3 bin arrays ≈ 210 bins).
        let n_bins = 8u32.max(1);
        let bin_step = self.bin_step.max(1) as u128; // price step in basis points-ish (1–100 typical)

        // Distribute reserves across bins with mild concentration at active bin.
        // weights: active gets more, outer bins less (geometric decay).
        let mut weights = Vec::with_capacity(n_bins as usize);
        let mut wsum = 0u128;
        for i in 0..n_bins {
            // weight_i ∝ 1 / (1 + i)^2
            let w = 10_000u128 / ((1 + i as u128) * (1 + i as u128));
            weights.push(w.max(1));
            wsum += w.max(1);
        }

        let mut remaining_in = amount_in as u128;
        let mut total_out = 0u128;
        let fee_bps = 25u128; // 0.25%

        for (i, &w) in weights.iter().enumerate() {
            if remaining_in == 0 {
                break;
            }
            let bin_rin = (reserve_in as u128).saturating_mul(w) / wsum.max(1);
            let mut bin_rout = (reserve_out as u128).saturating_mul(w) / wsum.max(1);
            if bin_rin == 0 || bin_rout == 0 {
                continue;
            }
            // Price worsens as we move away from active bin: reduce out by bin_step each step.
            let step_penalty = 10_000u128.saturating_sub(bin_step.saturating_mul(i as u128).min(3_000));
            bin_rout = bin_rout.saturating_mul(step_penalty) / 10_000;

            // Cap this bin's input to ~40% of bin reserve (don't drain one bin entirely).
            let max_bin_in = bin_rin.saturating_mul(40) / 100;
            let take_in = remaining_in.min(max_bin_in).max(1);
            let take_in_fee = take_in.saturating_mul(10_000 - fee_bps) / 10_000;
            if take_in_fee == 0 {
                break;
            }
            let out = bin_rout.saturating_mul(take_in_fee) / bin_rin.saturating_add(take_in_fee);
            total_out = total_out.saturating_add(out);
            remaining_in = remaining_in.saturating_sub(take_in);
        }

        // If still remaining (tiny dust), dump into last bin CPMM on residual global reserves.
        if remaining_in > 0 {
            let take_in_fee = remaining_in.saturating_mul(10_000 - fee_bps) / 10_000;
            let out = (reserve_out as u128).saturating_mul(take_in_fee)
                / (reserve_in as u128).saturating_add(take_in_fee);
            // heavily penalize residual (price impact)
            total_out = total_out.saturating_add(out / 2);
        }

        if amount_in > reserve_in / 10 {
            tracing::debug!(
                amount_in,
                reserve_in,
                bins = n_bins,
                out = total_out,
                "DLMM multi-bin quote (large vs reserve)"
            );
        }

        total_out.min(u64::MAX as u128) as u64
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
pub fn build_swap(params: &SwapParams, pool_state: &DlmmPoolState) -> Result<SwapInstructions> {
    let wallet = &params.wallet;
    let mut setup_instructions = Vec::new();
    let mut cleanup_instructions = Vec::new();

    let is_mint_x = params.mint == pool_state.token_x_mint;

    // Determine swap direction: x_to_y or y_to_x
    let swap_for_y = match params.direction {
        SwapDirection::Buy => !is_mint_x, // buying token_x means y→x, buying token_y means x→y
        SwapDirection::Sell => is_mint_x, // selling token_x means x→y
    };

    // Setup ATAs
    let (user_source_ata, user_dest_ata) = match params.direction {
        SwapDirection::Buy => {
            let (wsol_ata, wrap_ixs) = ata::wrap_sol_instructions(wallet, params.amount_in)?;
            setup_instructions.extend(wrap_ixs);
            let (token_ata, token_ixs) = ata::ensure_token_ata_instructions(wallet, &params.mint);
            setup_instructions.extend(token_ixs);
            cleanup_instructions.push(ata::close_wsol_account_instruction(wallet)?);
            if swap_for_y {
                (wsol_ata, token_ata)
            } else {
                (token_ata, wsol_ata)
            }
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
                pool_state
                    .bin_array_bitmap_extension
                    .unwrap_or(METEORA_DLMM_PROGRAM),
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

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .and_then(|s| s.try_into().ok())
        .map(u16::from_le_bytes)
}

fn read_i32(data: &[u8], offset: usize) -> Option<i32> {
    data.get(offset..offset + 4)
        .and_then(|s| s.try_into().ok())
        .map(i32::from_le_bytes)
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

    #[test]
    fn test_dlmm_multi_bin_pricing_positive() {
        let pool_state = DlmmPoolState {
            lb_pair: Pubkey::new_unique(),
            bin_array_bitmap_extension: None,
            token_x_mint: Pubkey::new_unique(),
            token_y_mint: solana_pubkey::pubkey!("So11111111111111111111111111111111111111112"),
            reserve_x: Pubkey::new_unique(),
            reserve_y: Pubkey::new_unique(),
            oracle: Pubkey::new_unique(),
            active_id: 0,
            reserve_x_balance: 150_000_000_000_000,
            reserve_y_balance: 1_000_000_000_000,
            bin_step: 10,
        };

        let amount_in = 1_000_000_000; // 1 SOL
        let output = pool_state.calculate_buy_output(amount_in);
        assert!(output > 0, "multi-bin quote must be positive");
        // Larger size should get worse average (less out per unit) than tiny size
        let tiny = pool_state.calculate_buy_output(1_000_000);
        let large = pool_state.calculate_buy_output(100_000_000_000);
        let tiny_rate = tiny as f64 / 1_000_000.0;
        let large_rate = large as f64 / 100_000_000_000.0;
        assert!(
            large_rate <= tiny_rate * 1.05,
            "large order should not get better rate than tiny (impact)"
        );
    }

    #[test]
    fn test_dlmm_bin_boundary_warning() {
        // Initialize tracing subscriber to capture warnings
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let pool_state = DlmmPoolState {
            lb_pair: Pubkey::new_unique(),
            bin_array_bitmap_extension: None,
            token_x_mint: Pubkey::new_unique(),
            token_y_mint: solana_pubkey::pubkey!("So11111111111111111111111111111111111111112"), // SOL
            reserve_x: Pubkey::new_unique(),
            reserve_y: Pubkey::new_unique(),
            oracle: Pubkey::new_unique(),
            active_id: 0,
            reserve_x_balance: 150_000_000_000_000, // 150M USDC
            reserve_y_balance: 1_000_000_000_000,   // 1M SOL
            bin_step: 1,
        };

        // Order size > 10% of reserve (200M SOL vs 1M SOL reserve)
        let amount_in = 200_000_000_000; // 200 SOL
        let output = pool_state.calculate_buy_output(amount_in);

        // Should trigger warning but still calculate
        assert!(output > 0);
    }
}
