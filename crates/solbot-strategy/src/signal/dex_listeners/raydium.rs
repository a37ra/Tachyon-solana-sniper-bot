//! Raydium pool creation listener (AMM v4, CP-MM, CLMM).
//!
//! Subscribes to Raydium program account updates via Geyser gRPC
//! with automatic reconnection and exponential backoff.

use solbot_common::types::Signal;
use crate::signal::geyser_client::{GeyserClient, GeyserConfig, AccountUpdate};
use crate::signal::signal_validator::SignalValidator;
use anyhow::{Context, Result};
use chrono::Utc;
use solana_pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_CPMM_PROGRAM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
pub const RAYDIUM_CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";

pub struct RaydiumListener {
    geyser_config: GeyserConfig,
    signal_validator: Arc<SignalValidator>,
}

impl Default for RaydiumListener {
    fn default() -> Self {
        Self::new()
    }
}

impl RaydiumListener {
    pub fn new() -> Self {
        Self {
            geyser_config: GeyserConfig::default(),
            signal_validator: Arc::new(SignalValidator::with_default()),
        }
    }

    pub fn with_geyser_config(config: GeyserConfig) -> Self {
        Self {
            geyser_config: config,
            signal_validator: Arc::new(SignalValidator::with_default()),
        }
    }

    pub fn with_validator(config: GeyserConfig, validator: Arc<SignalValidator>) -> Self {
        Self {
            geyser_config: config,
            signal_validator: validator,
        }
    }

    /// Subscribe to Raydium pool creation events across all 3 variants.
    pub async fn subscribe_to_pool_creation(
        &self,
    ) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(100);
        let geyser_config = self.geyser_config.clone();

        // Collect all Raydium program IDs
        let program_ids = vec![
            Pubkey::from_str(RAYDIUM_AMM_V4_PROGRAM)?,
            Pubkey::from_str(RAYDIUM_CPMM_PROGRAM)?,
            Pubkey::from_str(RAYDIUM_CLMM_PROGRAM)?,
        ];

        let geyser_client = GeyserClient::new(geyser_config);
        let mut account_updates = geyser_client
            .subscribe_to_programs(program_ids, None, false)
            .await
            .context("Failed to subscribe to Geyser gRPC")?;

        let validator = self.signal_validator.clone();

        tokio::spawn(async move {
            info!("Starting Raydium pool creation listener via Geyser gRPC");

            while let Some(update) = account_updates.recv().await {
                // Update current slot from account update
                validator.update_current_slot(update.slot);

                if let Some(signal) = Self::parse_raydium_account(update) {
                    // Validate signal age
                    match validator.validate_signal(&signal) {
                        Ok(true) => {
                            if tx.send(signal).await.is_err() {
                                info!("Receiver dropped, closing Raydium listener loop");
                                break;
                            }
                        }
                        Ok(false) => {
                            warn!(
                                mint = %signal.mint,
                                slot = signal.slot,
                                "Signal filtered by validator (too old)"
                            );
                        }
                        Err(e) => {
                            warn!(error = %e, mint = %signal.mint, "Error validating signal");
                        }
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Parse Raydium account update to extract pool creation signal.
    fn parse_raydium_account(update: AccountUpdate) -> Option<Signal> {
        let account_data = update.data;
        let owner_str = update.owner.to_string();

        // Raydium AMM v4 pool account size is 752 bytes
        if account_data.len() >= 752 && owner_str == RAYDIUM_AMM_V4_PROGRAM {
            if let Ok(coin_mint) = read_pubkey_bytes(&account_data, 400) {
                return Some(Signal {
                    mint: coin_mint.to_string(),
                    pool_address: update.account.to_string(),
                    dex_program: update.owner.to_string(),
                    signal_source: "geyser_grpc".to_string(),
                    slot: update.slot,
                    received_at: Utc::now().timestamp_millis() as u64,
                });
            }
        }

        // Raydium CP-MM pool account (~312 bytes)
        // Layout: discriminator(8) + pool_id(8) + authority(32) + token_0_mint(32) + token_1_mint(32)
        if account_data.len() >= 312 && owner_str == RAYDIUM_CPMM_PROGRAM {
            if let Ok(coin_mint) = read_pubkey_bytes(&account_data, 72) {
                return Some(Signal {
                    mint: coin_mint.to_string(),
                    pool_address: update.account.to_string(),
                    dex_program: update.owner.to_string(),
                    signal_source: "geyser_grpc".to_string(),
                    slot: update.slot,
                    received_at: Utc::now().timestamp_millis() as u64,
                });
            }
        }

        // Raydium CLMM pool account
        // Layout: discriminator(8) + pool_creator(32) + amm_config(32) + token_mint_0(32)
        if account_data.len() >= 104 && owner_str == RAYDIUM_CLMM_PROGRAM {
            if let Ok(coin_mint) = read_pubkey_bytes(&account_data, 72) {
                return Some(Signal {
                    mint: coin_mint.to_string(),
                    pool_address: update.account.to_string(),
                    dex_program: update.owner.to_string(),
                    signal_source: "geyser_grpc".to_string(),
                    slot: update.slot,
                    received_at: Utc::now().timestamp_millis() as u64,
                });
            }
        }

        None
    }
}

fn read_pubkey_bytes(data: &[u8], offset: usize) -> Result<Pubkey> {
    if offset + 32 > data.len() {
        anyhow::bail!("Pubkey offset out of bounds");
    }
    let bytes: [u8; 32] = data[offset..offset + 32].try_into()?;
    Ok(Pubkey::new_from_array(bytes))
}
