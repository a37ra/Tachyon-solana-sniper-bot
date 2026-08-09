//! Meteora DAMM v2 (CP-AMM) swap builder.
//!
//! Program ID: cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG
//!
//! Mainnet pool state size ≈ 1112 bytes (verified 2026-08-09):
//! - token_a_mint @ 168
//! - token_b_mint @ 200
//! - token_a_vault @ 232
//! - token_b_vault @ 264
//!
//! Accounts smaller than ~1100 bytes are not pool state (e.g. 408 B junk).

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use std::str::FromStr;

pub const METEORA_DAMM_V2_PROGRAM: Pubkey =
    solana_pubkey::pubkey!("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");

/// Anchor discriminator for `swap` (sha256("global:swap")[0..8]).
const SWAP_DISCRIMINATOR: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];

const POOL_SIZE_MIN: usize = 1100;
const POOL_SIZE_MAX: usize = 1200;
const WSOL: &str = "So11111111111111111111111111111111111111112";

#[derive(Debug, Clone)]
pub struct DammV2PoolState {
    pub pool_id: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
    pub token_a_vault_balance: u64,
    pub token_b_vault_balance: u64,
    /// Trade fee in 1e6 units (e.g. 2500 = 0.25%).
    pub trade_fee_rate: u64,
}

impl DammV2PoolState {
    pub fn from_account_data(data: &[u8], pool_address: &Pubkey) -> Result<Self> {
        if data.len() < POOL_SIZE_MIN || data.len() > POOL_SIZE_MAX {
            bail!(
                "DAMM v2 pool data unexpected size: {} (expected {}-{})",
                data.len(),
                POOL_SIZE_MIN,
                POOL_SIZE_MAX
            );
        }

        Ok(Self {
            pool_id: *pool_address,
            token_a_mint: read_pubkey(data, 168)?,
            token_b_mint: read_pubkey(data, 200)?,
            token_a_vault: read_pubkey(data, 232)?,
            token_b_vault: read_pubkey(data, 264)?,
            token_a_vault_balance: 0,
            token_b_vault_balance: 0,
            trade_fee_rate: 2500,
        })
    }

    pub fn with_vault_balances(mut self, a: u64, b: u64) -> Self {
        self.token_a_vault_balance = a;
        self.token_b_vault_balance = b;
        self
    }

    pub fn calculate_buy_output(&self, sol_amount: u64) -> u64 {
        let (rin, rout) = self.sol_token_reserves();
        self.cpmm_out(sol_amount, rin, rout)
    }

    pub fn calculate_sell_output(&self, token_amount: u64) -> u64 {
        let (rin_sol, rout_token) = self.sol_token_reserves();
        // sell token → SOL: in = token reserve, out = sol reserve
        self.cpmm_out(token_amount, rout_token, rin_sol)
    }

    fn sol_token_reserves(&self) -> (u64, u64) {
        let wsol = Pubkey::from_str(WSOL).unwrap_or_default();
        if self.token_b_mint == wsol {
            (self.token_b_vault_balance, self.token_a_vault_balance)
        } else if self.token_a_mint == wsol {
            (self.token_a_vault_balance, self.token_b_vault_balance)
        } else {
            (self.token_a_vault_balance, self.token_b_vault_balance)
        }
    }

    fn cpmm_out(&self, amount_in: u64, reserve_in: u64, reserve_out: u64) -> u64 {
        if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
            return 0;
        }
        const DENOM: u128 = 1_000_000;
        let fee = self.trade_fee_rate as u128;
        let amount_in_fee = (amount_in as u128)
            .saturating_mul(DENOM.saturating_sub(fee))
            / DENOM;
        if amount_in_fee == 0 {
            return 0;
        }
        let num = (reserve_out as u128).saturating_mul(amount_in_fee);
        let den = (reserve_in as u128).saturating_add(amount_in_fee);
        if den == 0 {
            return 0;
        }
        (num / den) as u64
    }
}

/// Build DAMM v2 swap instructions (constant-product style account layout).
pub fn build_swap(params: &SwapParams, pool: &DammV2PoolState) -> Result<SwapInstructions> {
    let wallet = &params.wallet;
    let mut setup = Vec::new();
    let mut cleanup = Vec::new();

    let wsol = Pubkey::from_str(WSOL)?;
    let is_a = params.mint == pool.token_a_mint;
    let is_b = params.mint == pool.token_b_mint;
    if !is_a && !is_b {
        bail!("mint {} is not a pool token", params.mint);
    }

    let (user_source, user_dest, source_vault, dest_vault) = match params.direction {
        SwapDirection::Buy => {
            let (wsol_ata, wrap_ixs) = ata::wrap_sol_instructions(wallet, params.amount_in)?;
            setup.extend(wrap_ixs);
            let (token_ata, token_ixs) = ata::ensure_token_ata_instructions(wallet, &params.mint);
            setup.extend(token_ixs);
            cleanup.push(ata::close_wsol_account_instruction(wallet)?);
            if is_a {
                // buy token A with SOL (B)
                (wsol_ata, token_ata, pool.token_b_vault, pool.token_a_vault)
            } else {
                (wsol_ata, token_ata, pool.token_a_vault, pool.token_b_vault)
            }
        }
        SwapDirection::Sell => {
            let token_ata = ata::derive_ata(wallet, &params.mint);
            let (wsol_ata, wsol_ixs) = ata::wrap_sol_instructions(wallet, 0)?;
            setup.extend(wsol_ixs);
            cleanup.push(ata::close_wsol_account_instruction(wallet)?);
            if is_a {
                (token_ata, wsol_ata, pool.token_a_vault, pool.token_b_vault)
            } else {
                (token_ata, wsol_ata, pool.token_b_vault, pool.token_a_vault)
            }
        }
    };

    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&SWAP_DISCRIMINATOR);
    data.extend_from_slice(&params.amount_in.to_le_bytes());
    data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());

    let swap_ix = Instruction {
        program_id: METEORA_DAMM_V2_PROGRAM,
        accounts: vec![
            AccountMeta::new(pool.pool_id, false),
            AccountMeta::new_readonly(*wallet, true),
            AccountMeta::new(user_source, false),
            AccountMeta::new(user_dest, false),
            AccountMeta::new(source_vault, false),
            AccountMeta::new(dest_vault, false),
            AccountMeta::new_readonly(pool.token_a_mint, false),
            AccountMeta::new_readonly(pool.token_b_mint, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(wsol, false),
        ],
        data,
    };

    Ok(SwapInstructions {
        setup_instructions: setup,
        swap_instructions: vec![swap_ix],
        cleanup_instructions: cleanup,
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
    fn rejects_small_non_pool_accounts() {
        let data = vec![0u8; 408];
        let err = DammV2PoolState::from_account_data(&data, &Pubkey::new_unique()).unwrap_err();
        assert!(err.to_string().contains("unexpected size"));
    }

    #[test]
    fn parses_1112_layout() {
        let mut data = vec![0u8; 1112];
        let mint_a = Pubkey::new_unique();
        let mint_b = Pubkey::from_str(WSOL).unwrap();
        let va = Pubkey::new_unique();
        let vb = Pubkey::new_unique();
        data[168..200].copy_from_slice(mint_a.as_ref());
        data[200..232].copy_from_slice(mint_b.as_ref());
        data[232..264].copy_from_slice(va.as_ref());
        data[264..296].copy_from_slice(vb.as_ref());
        let pool = DammV2PoolState::from_account_data(&data, &Pubkey::new_unique()).unwrap();
        assert_eq!(pool.token_a_mint, mint_a);
        assert_eq!(pool.token_b_mint, mint_b);
        assert_eq!(pool.token_a_vault, va);
        assert_eq!(pool.token_b_vault, vb);
    }

    #[test]
    fn pricing_nonzero() {
        let pool = DammV2PoolState {
            pool_id: Pubkey::new_unique(),
            token_a_mint: Pubkey::new_unique(),
            token_b_mint: Pubkey::from_str(WSOL).unwrap(),
            token_a_vault: Pubkey::new_unique(),
            token_b_vault: Pubkey::new_unique(),
            token_a_vault_balance: 1_000_000_000,
            token_b_vault_balance: 10_000_000_000,
            trade_fee_rate: 2500,
        };
        let out = pool.calculate_buy_output(1_000_000_000);
        assert!(out > 0);
    }

    #[test]
    fn build_swap_buy_ok() {
        let mint = Pubkey::new_unique();
        let pool = DammV2PoolState {
            pool_id: Pubkey::new_unique(),
            token_a_mint: mint,
            token_b_mint: Pubkey::from_str(WSOL).unwrap(),
            token_a_vault: Pubkey::new_unique(),
            token_b_vault: Pubkey::new_unique(),
            token_a_vault_balance: 1_000_000_000,
            token_b_vault_balance: 10_000_000_000,
            trade_fee_rate: 2500,
        };
        let params = SwapParams {
            mint,
            pool_address: pool.pool_id,
            dex_program_id: METEORA_DAMM_V2_PROGRAM,
            wallet: Pubkey::new_unique(),
            direction: SwapDirection::Buy,
            amount_in: 100_000_000,
            minimum_amount_out: 1,
            slippage_bps: 500,
        };
        let ix = build_swap(&params, &pool).unwrap();
        assert!(!ix.swap_instructions.is_empty());
    }
}
