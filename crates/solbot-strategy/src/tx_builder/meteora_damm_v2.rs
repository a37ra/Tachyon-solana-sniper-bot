//! Meteora DAMM v2 (CP-AMM) swap builder.
//!
//! Program: cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG
//!
//! Swap account layout from official IDL (`swap`):
//! 0 pool_authority (const HLnpSz…)
//! 1 pool (w)
//! 2 input_token_account (w)
//! 3 output_token_account (w)
//! 4 token_a_vault (w)
//! 5 token_b_vault (w)
//! 6 token_a_mint
//! 7 token_b_mint
//! 8 payer (signer)
//! 9 token_a_program
//! 10 token_b_program
//! 11 referral_token_account (optional w)
//! 12 event_authority
//! 13 program
//!
//! Pool state ≈ 1112 B: mints @168/200, vaults @232/264 (mainnet verified).

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use std::str::FromStr;

pub const METEORA_DAMM_V2_PROGRAM: Pubkey =
    solana_pubkey::pubkey!("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");

/// Const pool authority from IDL.
pub const POOL_AUTHORITY: Pubkey =
    solana_pubkey::pubkey!("HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC");

/// Anchor discriminator for `swap`.
const SWAP_DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

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
        let amount_in_fee =
            (amount_in as u128).saturating_mul(DENOM.saturating_sub(fee)) / DENOM;
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

/// Anchor event_authority PDA: seeds = [b"__event_authority"].
pub fn derive_event_authority() -> Pubkey {
    let (pk, _) =
        Pubkey::find_program_address(&[b"__event_authority"], &METEORA_DAMM_V2_PROGRAM);
    pk
}

pub fn build_swap(params: &SwapParams, pool: &DammV2PoolState) -> Result<SwapInstructions> {
    let wallet = &params.wallet;
    let mut setup = Vec::new();
    let mut cleanup = Vec::new();

    let is_a = params.mint == pool.token_a_mint;
    let is_b = params.mint == pool.token_b_mint;
    if !is_a && !is_b {
        bail!("mint {} is not a pool token", params.mint);
    }

    // token programs: prefer SPL Token; Token-2022 mints would need mint owner lookup.
    let token_a_program = spl_token::id();
    let token_b_program = spl_token::id();

    let (user_input, user_output) = match params.direction {
        SwapDirection::Buy => {
            let (wsol_ata, wrap_ixs) = ata::wrap_sol_instructions(wallet, params.amount_in)?;
            setup.extend(wrap_ixs);
            let (token_ata, token_ixs) = ata::ensure_token_ata_instructions(wallet, &params.mint);
            setup.extend(token_ixs);
            cleanup.push(ata::close_wsol_account_instruction(wallet)?);
            (wsol_ata, token_ata)
        }
        SwapDirection::Sell => {
            let token_ata = ata::derive_ata(wallet, &params.mint);
            let (wsol_ata, wsol_ixs) = ata::wrap_sol_instructions(wallet, 0)?;
            setup.extend(wsol_ixs);
            cleanup.push(ata::close_wsol_account_instruction(wallet)?);
            (token_ata, wsol_ata)
        }
    };

    // Vault order in IDL is always token_a_vault then token_b_vault (not input/output order).
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&SWAP_DISCRIMINATOR);
    data.extend_from_slice(&params.amount_in.to_le_bytes());
    data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());

    let event_authority = derive_event_authority();
    // Optional referral: pass program id as non-writable placeholder when unused
    // (Anchor optional accounts often use program id when None).
    let referral = METEORA_DAMM_V2_PROGRAM;

    let swap_ix = Instruction {
        program_id: METEORA_DAMM_V2_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(POOL_AUTHORITY, false),
            AccountMeta::new(pool.pool_id, false),
            AccountMeta::new(user_input, false),
            AccountMeta::new(user_output, false),
            AccountMeta::new(pool.token_a_vault, false),
            AccountMeta::new(pool.token_b_vault, false),
            AccountMeta::new_readonly(pool.token_a_mint, false),
            AccountMeta::new_readonly(pool.token_b_mint, false),
            AccountMeta::new_readonly(*wallet, true),
            AccountMeta::new_readonly(token_a_program, false),
            AccountMeta::new_readonly(token_b_program, false),
            AccountMeta::new(referral, false), // optional referral ATA; program id = none
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(METEORA_DAMM_V2_PROGRAM, false),
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
        assert!(DammV2PoolState::from_account_data(&data, &Pubkey::new_unique()).is_err());
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
        assert_eq!(pool.token_a_vault, va);
    }

    #[test]
    fn swap_account_count_matches_idl() {
        let mint = Pubkey::new_unique();
        let pool = DammV2PoolState {
            pool_id: Pubkey::new_unique(),
            token_a_mint: mint,
            token_b_mint: Pubkey::from_str(WSOL).unwrap(),
            token_a_vault: Pubkey::new_unique(),
            token_b_vault: Pubkey::new_unique(),
            token_a_vault_balance: 1e9 as u64,
            token_b_vault_balance: 10e9 as u64,
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
        assert_eq!(ix.swap_instructions[0].accounts.len(), 14);
        assert_eq!(ix.swap_instructions[0].accounts[0].pubkey, POOL_AUTHORITY);
        assert_eq!(
            ix.swap_instructions[0].data[..8],
            SWAP_DISCRIMINATOR
        );
    }

    #[test]
    fn event_authority_is_pda() {
        let ea = derive_event_authority();
        assert_ne!(ea, Pubkey::default());
    }
}
