//! Pump.fun bonding curve swap builder.
//!
//! Program ID: 6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2
//!
//! Pump.fun uses a bonding curve where price increases with supply.
//! Tokens start on the bonding curve and "graduate" (migrate) to
//! Raydium when the curve completes.
//!
//! Key states:
//! - Active bonding curve: buy/sell through Pump.fun program
//! - MIGRATING_PAUSE: graduation in progress, no trading
//! - Migrated: trading moves to Raydium pool (handled by raydium_amm_v4)

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_sdk::sysvar;

pub const PUMP_FUN_PROGRAM: Pubkey =
    solana_pubkey::pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2");

/// Global state account (PDA).
const GLOBAL_SEED: &[u8] = b"global";

/// Bonding curve account seed.
const BONDING_CURVE_SEED: &[u8] = b"bonding-curve";

/// Fee recipient for Pump.fun (known address).
pub const PUMP_FUN_FEE_RECIPIENT: Pubkey =
    solana_pubkey::pubkey!("CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbCJ7hziqrBnDY");

/// Buy instruction discriminator.
const BUY_DISCRIMINATOR: [u8; 8] = [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
/// Sell instruction discriminator.
const SELL_DISCRIMINATOR: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];

/// Bonding curve state parsed from on-chain data.
#[derive(Debug, Clone)]
pub struct PumpFunBondingCurve {
    pub bonding_curve: Pubkey,
    pub mint: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
}

impl PumpFunBondingCurve {
    pub fn from_account_data(data: &[u8], bonding_curve_address: &Pubkey) -> Result<Self> {
        if data.len() < 8 + 32 + 8 * 5 + 1 {
            bail!("Pump.fun bonding curve data too short: {} bytes", data.len());
        }

        let offset = 8; // discriminator
        let mint = read_pubkey(data, offset)?;

        // Derive the associated bonding curve (ATA of bonding curve for the mint)
        let associated = ata::derive_ata(bonding_curve_address, &mint);

        Ok(Self {
            bonding_curve: *bonding_curve_address,
            mint,
            associated_bonding_curve: associated,
            virtual_token_reserves: u64::from_le_bytes(
                data[offset + 32..offset + 40].try_into().unwrap_or([0; 8]),
            ),
            virtual_sol_reserves: u64::from_le_bytes(
                data[offset + 40..offset + 48].try_into().unwrap_or([0; 8]),
            ),
            real_token_reserves: u64::from_le_bytes(
                data[offset + 48..offset + 56].try_into().unwrap_or([0; 8]),
            ),
            real_sol_reserves: u64::from_le_bytes(
                data[offset + 56..offset + 64].try_into().unwrap_or([0; 8]),
            ),
            token_total_supply: u64::from_le_bytes(
                data[offset + 64..offset + 72].try_into().unwrap_or([0; 8]),
            ),
            complete: data.get(offset + 72).copied().unwrap_or(0) != 0,
        })
    }

    /// Check if the bonding curve is still active (not graduated/migrating).
    pub fn is_active(&self) -> bool {
        !self.complete
    }

    /// Calculate expected output for a buy (SOL → Token).
    pub fn calculate_buy_output(&self, sol_amount: u64) -> u64 {
        if self.virtual_sol_reserves == 0 || self.virtual_token_reserves == 0 {
            return 0;
        }
        // Constant product: (x + dx) * (y - dy) = x * y
        // dy = y * dx / (x + dx)
        let numerator = (self.virtual_token_reserves as u128) * (sol_amount as u128);
        let denominator = (self.virtual_sol_reserves as u128) + (sol_amount as u128);
        (numerator / denominator) as u64
    }

    /// Calculate expected output for a sell (Token → SOL).
    pub fn calculate_sell_output(&self, token_amount: u64) -> u64 {
        if self.virtual_sol_reserves == 0 || self.virtual_token_reserves == 0 {
            return 0;
        }
        let numerator = (self.virtual_sol_reserves as u128) * (token_amount as u128);
        let denominator = (self.virtual_token_reserves as u128) + (token_amount as u128);
        (numerator / denominator) as u64
    }
}

/// Derive the global state PDA.
pub fn derive_global() -> Pubkey {
    let (address, _) = Pubkey::find_program_address(&[GLOBAL_SEED], &PUMP_FUN_PROGRAM);
    address
}

/// Derive the bonding curve PDA for a mint.
pub fn derive_bonding_curve(mint: &Pubkey) -> Pubkey {
    let (address, _) = Pubkey::find_program_address(
        &[BONDING_CURVE_SEED, mint.as_ref()],
        &PUMP_FUN_PROGRAM,
    );
    address
}

/// Build Pump.fun buy/sell instructions.
pub fn build_swap(
    params: &SwapParams,
    curve: &PumpFunBondingCurve,
) -> Result<SwapInstructions> {
    if curve.complete {
        bail!("Bonding curve is complete (graduated) — use Raydium instead");
    }

    let wallet = &params.wallet;
    let mut setup_instructions = Vec::new();
    let cleanup_instructions = Vec::new();

    // Ensure user has an ATA for the token
    let (user_token_ata, ata_ixs) = ata::ensure_token_ata_instructions(wallet, &curve.mint);
    setup_instructions.extend(ata_ixs);

    let global = derive_global();

    let swap_ix = match params.direction {
        SwapDirection::Buy => {
            // Buy: SOL → Token
            let mut data = Vec::with_capacity(8 + 8 + 8);
            data.extend_from_slice(&BUY_DISCRIMINATOR);
            data.extend_from_slice(&params.amount_in.to_le_bytes());        // max SOL cost
            data.extend_from_slice(&params.minimum_amount_out.to_le_bytes()); // min tokens out

            Instruction {
                program_id: PUMP_FUN_PROGRAM,
                accounts: vec![
                    AccountMeta::new_readonly(global, false),
                    AccountMeta::new(PUMP_FUN_FEE_RECIPIENT, false),
                    AccountMeta::new_readonly(curve.mint, false),
                    AccountMeta::new(curve.bonding_curve, false),
                    AccountMeta::new(curve.associated_bonding_curve, false),
                    AccountMeta::new(user_token_ata, false),
                    AccountMeta::new(*wallet, true), // signer
                    AccountMeta::new_readonly(system_program::id(), false),
                    AccountMeta::new_readonly(spl_token::id(), false),
                    AccountMeta::new_readonly(sysvar::rent::id(), false),
                    AccountMeta::new_readonly(spl_associated_token_account::id(), false),
                ],
                data,
            }
        }
        SwapDirection::Sell => {
            // Sell: Token → SOL
            let mut data = Vec::with_capacity(8 + 8 + 8);
            data.extend_from_slice(&SELL_DISCRIMINATOR);
            data.extend_from_slice(&params.amount_in.to_le_bytes());        // token amount to sell
            data.extend_from_slice(&params.minimum_amount_out.to_le_bytes()); // min SOL out

            Instruction {
                program_id: PUMP_FUN_PROGRAM,
                accounts: vec![
                    AccountMeta::new_readonly(global, false),
                    AccountMeta::new(PUMP_FUN_FEE_RECIPIENT, false),
                    AccountMeta::new_readonly(curve.mint, false),
                    AccountMeta::new(curve.bonding_curve, false),
                    AccountMeta::new(curve.associated_bonding_curve, false),
                    AccountMeta::new(user_token_ata, false),
                    AccountMeta::new(*wallet, true), // signer
                    AccountMeta::new_readonly(system_program::id(), false),
                    AccountMeta::new_readonly(spl_associated_token_account::id(), false),
                    AccountMeta::new_readonly(spl_token::id(), false),
                ],
                data,
            }
        }
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
    fn test_bonding_curve_buy_calculation() {
        let curve = PumpFunBondingCurve {
            bonding_curve: Pubkey::new_unique(),
            mint: Pubkey::new_unique(),
            associated_bonding_curve: Pubkey::new_unique(),
            virtual_token_reserves: 1_000_000_000_000, // 1M tokens (6 decimals)
            virtual_sol_reserves: 30_000_000_000,        // 30 SOL
            real_token_reserves: 500_000_000_000,
            real_sol_reserves: 15_000_000_000,
            token_total_supply: 1_000_000_000_000,
            complete: false,
        };

        // Buy with 1 SOL
        let tokens_out = curve.calculate_buy_output(1_000_000_000);
        assert!(tokens_out > 0);
        // Rough check: ~32.26B tokens (1 / 31 * 1T)
        assert!(tokens_out > 30_000_000_000);
        assert!(tokens_out < 40_000_000_000);
    }

    #[test]
    fn test_graduated_curve_rejected() {
        let curve = PumpFunBondingCurve {
            bonding_curve: Pubkey::new_unique(),
            mint: Pubkey::new_unique(),
            associated_bonding_curve: Pubkey::new_unique(),
            virtual_token_reserves: 0,
            virtual_sol_reserves: 0,
            real_token_reserves: 0,
            real_sol_reserves: 0,
            token_total_supply: 0,
            complete: true,
        };

        let params = SwapParams {
            mint: curve.mint,
            pool_address: curve.bonding_curve,
            dex_program_id: PUMP_FUN_PROGRAM,
            wallet: Pubkey::new_unique(),
            direction: SwapDirection::Buy,
            amount_in: 1_000_000_000,
            minimum_amount_out: 0,
            slippage_bps: 500,
        };

        assert!(build_swap(&params, &curve).is_err());
    }
}
