//! Raydium CP-MM (Concentrated Pool Market Maker) swap instruction builder.
//!
//! Program ID: CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C
//!
//! CP-MM is Raydium's newer constant-product AMM without OpenBook dependency.
//! Simpler account layout than AMM v4.

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub const RAYDIUM_CPMM_PROGRAM: Pubkey =
    solana_pubkey::pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");

/// Swap instruction discriminator for CP-MM.
/// Anchor discriminator: hash("global:swap_base_in")[..8]
const SWAP_BASE_IN_DISCRIMINATOR: [u8; 8] = [0x8f, 0x25, 0x31, 0x82, 0x74, 0xd3, 0x9c, 0x85];

/// CP-MM pool state accounts parsed from on-chain data.
#[derive(Debug, Clone)]
pub struct CpmmPoolState {
    pub pool_id: Pubkey,
    pub authority: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub observation_key: Pubkey,
}

impl CpmmPoolState {
    /// Parse from on-chain account data.
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        // Anchor discriminator (8 bytes) + various fields
        if data.len() < 8 + 32 * 8 {
            bail!("CP-MM account data too short: {} bytes", data.len());
        }

        let offset = 8; // skip anchor discriminator
        Ok(Self {
            pool_id: read_pubkey(data, offset)?,
            authority: read_pubkey(data, offset + 32)?,
            token_0_mint: read_pubkey(data, offset + 32 * 2)?,
            token_1_mint: read_pubkey(data, offset + 32 * 3)?,
            token_0_vault: read_pubkey(data, offset + 32 * 4)?,
            token_1_vault: read_pubkey(data, offset + 32 * 5)?,
            lp_mint: read_pubkey(data, offset + 32 * 6)?,
            observation_key: read_pubkey(data, offset + 32 * 7)?,
        })
    }
}

/// Build Raydium CP-MM swap instructions.
pub fn build_swap(
    params: &SwapParams,
    pool_state: &CpmmPoolState,
) -> Result<SwapInstructions> {
    let wallet = &params.wallet;
    let mut setup_instructions = Vec::new();
    let mut cleanup_instructions = Vec::new();

    // Determine which vault is input and which is output based on direction
    let is_mint_token0 = params.mint == pool_state.token_0_mint;

    let (input_vault, output_vault, input_mint, output_mint) = match params.direction {
        SwapDirection::Buy => {
            // SOL → Token: input = SOL side, output = token side
            if is_mint_token0 {
                (&pool_state.token_1_vault, &pool_state.token_0_vault,
                 &pool_state.token_1_mint, &pool_state.token_0_mint)
            } else {
                (&pool_state.token_0_vault, &pool_state.token_1_vault,
                 &pool_state.token_0_mint, &pool_state.token_1_mint)
            }
        }
        SwapDirection::Sell => {
            // Token → SOL: input = token side, output = SOL side
            if is_mint_token0 {
                (&pool_state.token_0_vault, &pool_state.token_1_vault,
                 &pool_state.token_0_mint, &pool_state.token_1_mint)
            } else {
                (&pool_state.token_1_vault, &pool_state.token_0_vault,
                 &pool_state.token_1_mint, &pool_state.token_0_mint)
            }
        }
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

    // Build instruction data: discriminator + amount_in (u64) + minimum_amount_out (u64)
    let mut instruction_data = Vec::with_capacity(24);
    instruction_data.extend_from_slice(&SWAP_BASE_IN_DISCRIMINATOR);
    instruction_data.extend_from_slice(&params.amount_in.to_le_bytes());
    instruction_data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());

    let swap_ix = Instruction {
        program_id: RAYDIUM_CPMM_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(*wallet, true),      // payer / signer
            AccountMeta::new_readonly(pool_state.authority, false),
            AccountMeta::new(pool_state.pool_id, false),    // pool state
            AccountMeta::new(user_input_ata, false),        // user input token account
            AccountMeta::new(user_output_ata, false),       // user output token account
            AccountMeta::new(*input_vault, false),          // pool input vault
            AccountMeta::new(*output_vault, false),         // pool output vault
            AccountMeta::new_readonly(*input_mint, false),
            AccountMeta::new_readonly(*output_mint, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new(pool_state.observation_key, false),
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
