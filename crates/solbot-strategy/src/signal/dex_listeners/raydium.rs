//! Raydium pool creation listener (AMM v4, CP-MM, CLMM).
//!
//! Subscribes to Raydium program account updates via Geyser gRPC
//! with automatic reconnection and exponential backoff.

use crate::signal::geyser_client::{AccountUpdate, GeyserClient, GeyserConfig};
use crate::signal::helius_ws_client::{HeliusWsClient, HeliusWsConfig};
use crate::signal::signal_validator::SignalValidator;
use anyhow::{Context, Result};
use chrono::Utc;
use solana_pubkey::Pubkey;
use solbot_common::types::Signal;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_CPMM_PROGRAM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
pub const RAYDIUM_CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";

pub struct RaydiumListener {
    geyser_config: GeyserConfig,
    helius_ws_config: HeliusWsConfig,
    signal_source: String,
    signal_validator: Arc<SignalValidator>,
}

impl Default for RaydiumListener {
    fn default() -> Self {
        Self::new()
    }
}

impl RaydiumListener {
    pub fn new() -> Self {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "helius_ws".to_string());

        Self {
            geyser_config: GeyserConfig::default(),
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: Arc::new(SignalValidator::with_default()),
        }
    }

    pub fn with_geyser_config(config: GeyserConfig) -> Self {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "helius_ws".to_string());

        Self {
            geyser_config: config,
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: Arc::new(SignalValidator::with_default()),
        }
    }

    pub fn with_validator(config: GeyserConfig, validator: Arc<SignalValidator>) -> Self {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "helius_ws".to_string());

        Self {
            geyser_config: config,
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: validator,
        }
    }

    pub fn with_signal_source(
        geyser_config: GeyserConfig,
        helius_ws_config: HeliusWsConfig,
        signal_source: String,
        validator: Arc<SignalValidator>,
    ) -> Self {
        Self {
            geyser_config,
            helius_ws_config,
            signal_source,
            signal_validator: validator,
        }
    }

    /// Subscribe to Raydium pool creation events across all 3 variants.
    pub async fn subscribe_to_pool_creation(&self) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(100);
        let geyser_config = self.geyser_config.clone();
        let helius_ws_config = self.helius_ws_config.clone();
        let signal_source = self.signal_source.clone();

        // Collect all Raydium program IDs
        let program_ids = vec![
            Pubkey::from_str(RAYDIUM_AMM_V4_PROGRAM)?,
            Pubkey::from_str(RAYDIUM_CPMM_PROGRAM)?,
            Pubkey::from_str(RAYDIUM_CLMM_PROGRAM)?,
        ];

        let validator = self.signal_validator.clone();

        tokio::spawn(async move {
            let mut account_updates = if signal_source == "helius_ws" {
                info!("Starting Raydium pool creation listener via Helius WebSocket");
                let helius_client = HeliusWsClient::new(helius_ws_config);
                helius_client
                    .subscribe_to_programs(program_ids)
                    .await
                    .context("Failed to subscribe to Helius WebSocket")
                    .unwrap()
            } else {
                info!("Starting Raydium pool creation listener via Geyser gRPC");
                let geyser_client = GeyserClient::new(geyser_config);
                geyser_client
                    .subscribe_to_programs(program_ids, None, false)
                    .await
                    .context("Failed to subscribe to Geyser gRPC")
                    .unwrap()
            };

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
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "helius_ws".to_string());
        let account_data = update.data;
        let owner_str = update.owner.to_string();

        // Raydium AMM v4 pool account size is 752 bytes
        if account_data.len() >= 752 && owner_str == RAYDIUM_AMM_V4_PROGRAM {
            if let Ok(coin_mint) = read_pubkey_bytes(&account_data, 400) {
                return Some(Signal {
                    mint: coin_mint.to_string(),
                    pool_address: update.account.to_string(),
                    dex_program: update.owner.to_string(),
                    signal_source: signal_source.clone(),
                    slot: update.slot,
                    received_at: update.timestamp_ms,
                    raw_pool_account_data: Some(account_data.clone()),
                    raw_pool_account_owner: Some(update.owner.to_string()),
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
                    signal_source: signal_source.clone(),
                    slot: update.slot,
                    received_at: update.timestamp_ms,
                    raw_pool_account_data: Some(account_data.clone()),
                    raw_pool_account_owner: Some(update.owner.to_string()),
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
                    signal_source: signal_source.clone(),
                    slot: update.slot,
                    received_at: update.timestamp_ms,
                    raw_pool_account_data: Some(account_data.clone()),
                    raw_pool_account_owner: Some(update.owner.to_string()),
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
