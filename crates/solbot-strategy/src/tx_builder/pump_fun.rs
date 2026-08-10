//! Pump.fun bonding curve swap builder.
//!
//! Program ID: 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
//!
//! ## BondingCurve account layout (verified mainnet dump 2026-08-10)
//! ```text
//! 0..8   disc
//! 8..16  virtual_token_reserves
//! 16..24 virtual_sol_reserves
//! 24..32 real_token_reserves
//! 32..40 real_sol_reserves
//! 40..48 token_total_supply
//! 48     complete: bool
//! 49..81 creator: Pubkey
//! ```
//! Mint is NOT in account data.
//!
//! ## Sell/Buy accounts (pump-public-docs IDL)
//! Sell: global, fee_recipient, mint, bonding_curve, associated_bonding_curve,
//! associated_user, user, system, creator_vault, token_program, event_authority,
//! program, fee_config, fee_program.

use super::{ata, SwapDirection, SwapInstructions, SwapParams};
use anyhow::{anyhow, bail, Result};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk_ids::system_program;
use std::sync::OnceLock;
use tracing::info;

pub const PUMP_FUN_PROGRAM: Pubkey =
    solana_pubkey::pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");

/// Pump fee program (hosts fee_config PDA).
pub const PUMP_FEE_PROGRAM: Pubkey =
    solana_pubkey::pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");

const GLOBAL_SEED: &[u8] = b"global";
const BONDING_CURVE_SEED: &[u8] = b"bonding-curve";
const CREATOR_VAULT_SEED: &[u8] = b"creator-vault";
const EVENT_AUTHORITY_SEED: &[u8] = b"__event_authority";
const FEE_CONFIG_SEED: &[u8] = b"fee_config";

/// Second seed for fee_config PDA (from official IDL const bytes).
const FEE_CONFIG_CONST_SEED: [u8; 32] = [
    1, 86, 224, 246, 147, 102, 90, 207, 68, 219, 21, 104, 191, 23, 91, 170, 81, 137, 203, 151,
    245, 210, 255, 59, 101, 93, 43, 182, 253, 109, 24, 176,
];

/// Loaded at process start from on-chain Global (`fee_recipient` field).
/// Never hardcode — previous stale constants caused Custom(6000) NotAuthorized.
static FEE_RECIPIENT: OnceLock<Pubkey> = OnceLock::new();

const BUY_DISCRIMINATOR: [u8; 8] = [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
const SELL_DISCRIMINATOR: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];

pub const BONDING_CURVE_MIN_LEN: usize = 81; // through creator pubkey

#[derive(Debug, Clone)]
pub struct PumpFunBondingCurve {
    pub bonding_curve: Pubkey,
    pub mint: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub creator: Pubkey,
    pub virtual_token_reserves: u64,
    pub virtual_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
}

impl PumpFunBondingCurve {
    pub fn from_account_data(
        data: &[u8],
        bonding_curve_address: &Pubkey,
        mint: &Pubkey,
    ) -> Result<Self> {
        if data.len() < BONDING_CURVE_MIN_LEN {
            bail!(
                "Pump.fun bonding curve data too short: {} bytes (need ≥{})",
                data.len(),
                BONDING_CURVE_MIN_LEN
            );
        }

        let (expected_pda, _) =
            Pubkey::find_program_address(&[BONDING_CURVE_SEED, mint.as_ref()], &PUMP_FUN_PROGRAM);
        if expected_pda != *bonding_curve_address {
            bail!(
                "Bonding curve PDA mismatch for mint {} (expected {}, got {})",
                mint,
                expected_pda,
                bonding_curve_address
            );
        }

        Self::from_account_data_unchecked(data, bonding_curve_address, mint)
    }

    pub fn from_account_data_unchecked(
        data: &[u8],
        bonding_curve_address: &Pubkey,
        mint: &Pubkey,
    ) -> Result<Self> {
        if data.len() < BONDING_CURVE_MIN_LEN {
            bail!("Pump.fun bonding curve data too short: {} bytes", data.len());
        }
        let creator = Pubkey::new_from_array(data[49..81].try_into().unwrap());
        let associated = ata::derive_ata_with_program(
            bonding_curve_address,
            mint,
            &ata::TOKEN_2022_PROGRAM_ID,
        );
        Ok(Self {
            bonding_curve: *bonding_curve_address,
            mint: *mint,
            associated_bonding_curve: associated,
            creator,
            virtual_token_reserves: u64::from_le_bytes(data[8..16].try_into().unwrap()),
            virtual_sol_reserves: u64::from_le_bytes(data[16..24].try_into().unwrap()),
            real_token_reserves: u64::from_le_bytes(data[24..32].try_into().unwrap()),
            real_sol_reserves: u64::from_le_bytes(data[32..40].try_into().unwrap()),
            token_total_supply: u64::from_le_bytes(data[40..48].try_into().unwrap()),
            complete: data[48] != 0,
        })
    }

    pub fn is_active(&self) -> bool {
        !self.complete
    }

    pub fn calculate_buy_output(&self, sol_amount: u64) -> u64 {
        if self.virtual_sol_reserves == 0 || self.virtual_token_reserves == 0 {
            return 0;
        }
        let numerator = (self.virtual_token_reserves as u128) * (sol_amount as u128);
        let denominator = (self.virtual_sol_reserves as u128) + (sol_amount as u128);
        (numerator / denominator) as u64
    }

    pub fn calculate_sell_output(&self, token_amount: u64) -> u64 {
        if self.virtual_sol_reserves == 0 || self.virtual_token_reserves == 0 {
            return 0;
        }
        let numerator = (self.virtual_sol_reserves as u128) * (token_amount as u128);
        let denominator = (self.virtual_token_reserves as u128) + (token_amount as u128);
        (numerator / denominator) as u64
    }
}

pub fn derive_global() -> Pubkey {
    Pubkey::find_program_address(&[GLOBAL_SEED], &PUMP_FUN_PROGRAM).0
}

/// Borsh layout of Global after 8-byte disc (pump IDL):
/// `initialized: bool`, `authority: Pubkey`, **`fee_recipient: Pubkey`**, …
const GLOBAL_FEE_RECIPIENT_OFFSET: usize = 8 + 1 + 32; // disc + bool + authority

/// Fetch Global and cache `fee_recipient`. **Call once at startup** (paper and live).
/// Refuses silent hardcode fallback — missing load → build_swap fails closed.
pub async fn load_fee_recipient_from_global(rpc: &RpcClient) -> Result<Pubkey> {
    let global = derive_global();
    let acc = rpc
        .get_account(&global)
        .await
        .map_err(|e| anyhow!("get Global {global}: {e}"))?;
    if acc.owner != PUMP_FUN_PROGRAM {
        bail!(
            "Global account owner mismatch: expected pump program, got {}",
            acc.owner
        );
    }
    let data = &acc.data;
    if data.len() < GLOBAL_FEE_RECIPIENT_OFFSET + 32 {
        bail!(
            "Global account too short: {} bytes (need ≥{})",
            data.len(),
            GLOBAL_FEE_RECIPIENT_OFFSET + 32
        );
    }
    let fee = Pubkey::new_from_array(
        data[GLOBAL_FEE_RECIPIENT_OFFSET..GLOBAL_FEE_RECIPIENT_OFFSET + 32]
            .try_into()
            .unwrap(),
    );
    if fee == Pubkey::default() {
        bail!("Global.fee_recipient is default pubkey");
    }
    match FEE_RECIPIENT.set(fee) {
        Ok(()) => {
            info!(
                fee_recipient = %fee,
                global = %global,
                "pump fee_recipient loaded from on-chain Global"
            );
        }
        Err(existing) => {
            if existing != fee {
                bail!(
                    "fee_recipient already set to {existing}, Global now has {fee} — restart required"
                );
            }
        }
    }
    Ok(fee)
}

/// Cached fee_recipient from [`load_fee_recipient_from_global`].
pub fn fee_recipient() -> Result<Pubkey> {
    FEE_RECIPIENT.get().copied().ok_or_else(|| {
        anyhow!(
            "pump fee_recipient not loaded — call load_fee_recipient_from_global at startup \
             (no hardcoded fallback)"
        )
    })
}

/// Inject fee_recipient without RPC (unit tests / offline smoke only).
/// Production path must use [`load_fee_recipient_from_global`].
pub fn inject_fee_recipient_for_tests(pk: Pubkey) {
    let _ = FEE_RECIPIENT.set(pk);
}

pub fn derive_bonding_curve(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[BONDING_CURVE_SEED, mint.as_ref()], &PUMP_FUN_PROGRAM).0
}

pub fn derive_creator_vault(creator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[CREATOR_VAULT_SEED, creator.as_ref()], &PUMP_FUN_PROGRAM).0
}

pub fn derive_event_authority() -> Pubkey {
    Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], &PUMP_FUN_PROGRAM).0
}

pub fn derive_fee_config() -> Pubkey {
    Pubkey::find_program_address(
        &[FEE_CONFIG_SEED, &FEE_CONFIG_CONST_SEED],
        &PUMP_FEE_PROGRAM,
    )
    .0
}

pub fn build_swap(params: &SwapParams, curve: &PumpFunBondingCurve) -> Result<SwapInstructions> {
    if curve.complete {
        bail!("Bonding curve is complete (graduated) — use Raydium instead");
    }
    if curve.virtual_token_reserves == 0 || curve.virtual_sol_reserves == 0 {
        bail!("Bonding curve has zero virtual reserves");
    }

    let fee_recipient = fee_recipient()?;

    let wallet = &params.wallet;
    let token_program = ata::TOKEN_2022_PROGRAM_ID;
    let (user_token_ata, ata_ixs) =
        ata::ensure_token_ata_instructions_with_program(wallet, &curve.mint, &token_program);

    let global = derive_global();
    let creator_vault = derive_creator_vault(&curve.creator);
    let event_authority = derive_event_authority();
    let fee_config = derive_fee_config();
    let associated_bc = curve.associated_bonding_curve;

    let swap_ix = match params.direction {
        SwapDirection::Buy => {
            // IDL buy args: amount, max_sol_cost, track_volume (bool as u8)
            let mut data = Vec::with_capacity(8 + 8 + 8 + 1);
            data.extend_from_slice(&BUY_DISCRIMINATOR);
            data.extend_from_slice(&params.minimum_amount_out.to_le_bytes()); // amount (tokens)
            data.extend_from_slice(&params.amount_in.to_le_bytes()); // max_sol_cost
            data.push(0); // track_volume = false

            Instruction {
                program_id: PUMP_FUN_PROGRAM,
                accounts: sell_style_accounts(
                    global,
                    fee_recipient,
                    curve,
                    associated_bc,
                    user_token_ata,
                    *wallet,
                    creator_vault,
                    token_program,
                    event_authority,
                    fee_config,
                ),
                data,
            }
        }
        SwapDirection::Sell => {
            let mut data = Vec::with_capacity(8 + 8 + 8);
            data.extend_from_slice(&SELL_DISCRIMINATOR);
            data.extend_from_slice(&params.amount_in.to_le_bytes());
            data.extend_from_slice(&params.minimum_amount_out.to_le_bytes());

            Instruction {
                program_id: PUMP_FUN_PROGRAM,
                accounts: sell_style_accounts(
                    global,
                    fee_recipient,
                    curve,
                    associated_bc,
                    user_token_ata,
                    *wallet,
                    creator_vault,
                    token_program,
                    event_authority,
                    fee_config,
                ),
                data,
            }
        }
    };

    Ok(SwapInstructions {
        setup_instructions: ata_ixs,
        swap_instructions: vec![swap_ix],
        cleanup_instructions: vec![],
        additional_signers: vec![],
    })
}

/// Shared account metas matching IDL sell (and buy core without volume trackers).
fn sell_style_accounts(
    global: Pubkey,
    fee_recipient: Pubkey,
    curve: &PumpFunBondingCurve,
    associated_bc: Pubkey,
    user_token_ata: Pubkey,
    wallet: Pubkey,
    creator_vault: Pubkey,
    token_program: Pubkey,
    event_authority: Pubkey,
    fee_config: Pubkey,
) -> Vec<AccountMeta> {
    // IDL sell order:
    // 0 global, 1 fee_recipient, 2 mint, 3 bonding_curve, 4 associated_bonding_curve,
    // 5 associated_user, 6 user, 7 system, 8 creator_vault, 9 token_program,
    // 10 event_authority, 11 program, 12 fee_config, 13 fee_program
    vec![
        AccountMeta::new_readonly(global, false),
        AccountMeta::new(fee_recipient, false),
        AccountMeta::new_readonly(curve.mint, false),
        AccountMeta::new(curve.bonding_curve, false),
        AccountMeta::new(associated_bc, false),
        AccountMeta::new(user_token_ata, false),
        AccountMeta::new(wallet, true),
        AccountMeta::new_readonly(system_program::id(), false),
        AccountMeta::new(creator_vault, false),
        AccountMeta::new_readonly(token_program, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(PUMP_FUN_PROGRAM, false),
        AccountMeta::new_readonly(fee_config, false),
        AccountMeta::new_readonly(PUMP_FEE_PROGRAM, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    const MAINNET_CURVE_HEX: &str = "17b7f83760d8ac600010d847e3cf030001ac23fc060000000078c5fb51d1020001000000000000000080c6a47e8d03000014c6624f38a8b6555e22d12ed5197fb9cf1ef2f5da17665e9c8aaec3e24ea70c00000000000000000000000000000000000000000000000000000000000000000000";

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn ensure_fee_recipient() {
        inject_fee_recipient_for_tests(
            Pubkey::from_str("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV").unwrap(),
        );
    }

    #[test]
    fn mainnet_curve_layout_active_not_complete() {
        ensure_fee_recipient();
        let data = hex_decode(MAINNET_CURVE_HEX);
        assert_eq!(data.len(), 115);
        assert_eq!(data[48], 0);
        let mint = Pubkey::new_unique();
        let curve_addr = Pubkey::new_unique();
        let c = PumpFunBondingCurve::from_account_data_unchecked(&data, &curve_addr, &mint).unwrap();
        assert!(!c.complete);
        assert_eq!(c.virtual_token_reserves, 1_073_000_000_000_000);
        assert_ne!(c.creator, Pubkey::default());
        let ix = build_swap(
            &SwapParams {
                mint,
                pool_address: curve_addr,
                dex_program_id: PUMP_FUN_PROGRAM,
                wallet: Pubkey::new_unique(),
                direction: SwapDirection::Sell,
                amount_in: 1_000_000,
                minimum_amount_out: 1,
                slippage_bps: 1000,
            },
            &c,
        )
        .unwrap();
        // setup ATA + sell with 14 accounts
        assert_eq!(ix.swap_instructions[0].accounts.len(), 14);
    }

    #[test]
    fn sell_account_count_matches_idl() {
        ensure_fee_recipient();
        let mint = Pubkey::new_unique();
        let curve_addr = derive_bonding_curve(&mint);
        let mut data = vec![0u8; 115];
        data[8..16].copy_from_slice(&1_000_000u64.to_le_bytes());
        data[16..24].copy_from_slice(&30_000_000_000u64.to_le_bytes());
        let creator = Pubkey::new_unique();
        data[49..81].copy_from_slice(creator.as_ref());
        let c = PumpFunBondingCurve::from_account_data_unchecked(&data, &curve_addr, &mint).unwrap();
        let ix = build_swap(
            &SwapParams {
                mint,
                pool_address: curve_addr,
                dex_program_id: PUMP_FUN_PROGRAM,
                wallet: Pubkey::new_unique(),
                direction: SwapDirection::Sell,
                amount_in: 1,
                minimum_amount_out: 1,
                slippage_bps: 100,
            },
            &c,
        )
        .unwrap();
        assert_eq!(ix.swap_instructions[0].accounts.len(), 14);
        // [8] creator_vault, [10] event_authority, [13] fee_program
        assert_eq!(
            ix.swap_instructions[0].accounts[13].pubkey,
            PUMP_FEE_PROGRAM
        );
        assert_eq!(
            ix.swap_instructions[0].accounts[11].pubkey,
            PUMP_FUN_PROGRAM
        );
    }

    #[test]
    fn old_complete_offset_80_false_positive_documented() {
        let data = hex_decode(MAINNET_CURVE_HEX);
        assert_ne!(data.get(80).copied().unwrap_or(0), 0);
        assert_eq!(data[48], 0);
    }

    #[test]
    fn build_swap_rejects_complete_curve() {
        ensure_fee_recipient();
        let mint = Pubkey::new_unique();
        let curve_addr = derive_bonding_curve(&mint);
        let curve = PumpFunBondingCurve {
            bonding_curve: curve_addr,
            mint,
            associated_bonding_curve: Pubkey::new_unique(),
            creator: Pubkey::new_unique(),
            virtual_token_reserves: 1,
            virtual_sol_reserves: 1,
            real_token_reserves: 0,
            real_sol_reserves: 0,
            token_total_supply: 1,
            complete: true,
        };
        assert!(build_swap(
            &SwapParams {
                mint,
                pool_address: curve_addr,
                dex_program_id: PUMP_FUN_PROGRAM,
                wallet: Pubkey::new_unique(),
                direction: SwapDirection::Sell,
                amount_in: 1,
                minimum_amount_out: 1,
                slippage_bps: 100,
            },
            &curve
        )
        .is_err());
    }

    #[test]
    fn pda_mismatch_rejected() {
        let data = hex_decode(MAINNET_CURVE_HEX);
        let mint = Pubkey::from_str("tABuuUJuNnzxv3wXx8wPvpwfcaPWaBpu4o7HDnypump").unwrap();
        assert!(
            PumpFunBondingCurve::from_account_data(&data, &Pubkey::new_unique(), &mint).is_err()
        );
    }

    #[test]
    fn fee_config_pda_is_stable() {
        let a = derive_fee_config();
        let b = derive_fee_config();
        assert_eq!(a, b);
    }
}
