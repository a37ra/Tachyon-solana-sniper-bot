//! Meteora Dynamic AMM swap builder.
//!
//! Program ID: Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB
//!
//! Dynamic AMM is Meteora's constant-product AMM with variable fees
//! that adjust based on market volatility.

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

pub const METEORA_DYNAMIC_AMM_PROGRAM: Pubkey =
    solana_sdk::pubkey!("Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB");

/// Anchor discriminator for `swap` instruction.
const SWAP_DISCRIMINATOR: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];

/// Dynamic AMM pool state parsed from on-chain data.
#[derive(Debug, Clone)]
pub struct DynamicAmmPoolState {
    pub pool_id: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub a_vault: Pubkey,
    pub b_vault: Pubkey,
    pub a_token_vault: Pubkey,
    pub b_token_vault: Pubkey,
    pub a_vault_lp_mint: Pubkey,
    pub b_vault_lp_mint: Pubkey,
    pub a_vault_lp: Pubkey,
    pub b_vault_lp: Pubkey,
    pub admin_token_a_fee: Pubkey,
    pub admin_token_b_fee: Pubkey,
}

impl DynamicAmmPoolState {
    pub fn from_account_data(data: &[u8], pool_address: &Pubkey) -> Result<Self> {
        if data.len() < 8 + 32 * 13 {
            bail!("Dynamic AMM account data too short: {} bytes", data.len());
        }

        let offset = 8; // anchor discriminator
        Ok(Self {
            pool_id: *pool_address,
            token_a_mint: read_pubkey(data, offset)?,
            token_b_mint: read_pubkey(data, offset + 32)?,
            a_vault: read_pubkey(data, offset + 64)?,
            b_vault: read_pubkey(data, offset + 96)?,
            a_token_vault: read_pubkey(data, offset + 128)?,
            b_token_vault: read_pubkey(data, offset + 160)?,
            a_vault_lp_mint: read_pubkey(data, offset + 192)?,
            b_vault_lp_mint: read_pubkey(data, offset + 224)?,
            a_vault_lp: read_pubkey(data, offset + 256)?,
            b_vault_lp: read_pubkey(data, offset + 288)?,
            admin_token_a_fee: read_pubkey(data, offset + 320)?,
            admin_token_b_fee: read_pubkey(data, offset + 352)?,
        })
    }
}

/// Build Meteora Dynamic AMM swap instructions.
pub fn build_swap(
    params: &SwapParams,
    pool_state: &DynamicAmmPoolState,
) -> Result<SwapInstructions> {
    let wallet = &params.wallet;
    let mut setup_instructions = Vec::new();
    let mut cleanup_instructions = Vec::new();

    let is_mint_a = params.mint == pool_state.token_a_mint;

    // Setup ATAs
    let (user_source_ata, user_dest_ata) = match params.direction {
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

    // Build instruction data
    let mut instruction_data = Vec::with_capacity(8 + 8 + 8);
    instruction_data.extend_from_slice(&SWAP_DISCRIMINATOR);
    instruction_data.extend_from_slice(&params.amount_in.to_le_bytes());
    instruction_data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());

    let swap_ix = Instruction {
        program_id: METEORA_DYNAMIC_AMM_PROGRAM,
        accounts: vec![
            AccountMeta::new(pool_state.pool_id, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new(pool_state.a_vault, false),
            AccountMeta::new(pool_state.b_vault, false),
            AccountMeta::new(pool_state.a_token_vault, false),
            AccountMeta::new(pool_state.b_token_vault, false),
            AccountMeta::new(pool_state.a_vault_lp_mint, false),
            AccountMeta::new(pool_state.b_vault_lp_mint, false),
            AccountMeta::new(pool_state.a_vault_lp, false),
            AccountMeta::new(pool_state.b_vault_lp, false),
            AccountMeta::new(pool_state.admin_token_a_fee, false),
            AccountMeta::new(pool_state.admin_token_b_fee, false),
            AccountMeta::new_readonly(*wallet, true),
            AccountMeta::new(user_source_ata, false),
            AccountMeta::new(user_dest_ata, false),
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
