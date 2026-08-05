//! Meteora pool creation listener (DLMM, Dynamic AMM).
//!
//! Subscribes to Meteora DLMM / Dynamic AMM program accounts via Geyser gRPC
//! with automatic reconnection and exponential backoff.

use solbot_common::types::Signal;
use crate::signal::geyser_client::{GeyserClient, GeyserConfig, AccountUpdate};
use crate::signal::helius_ws_client::{HeliusWsClient, HeliusWsConfig};
use crate::signal::signal_validator::SignalValidator;
use anyhow::{Context, Result};
use chrono::Utc;
use solana_pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub const METEORA_DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
pub const METEORA_DYNAMIC_PROGRAM: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";

pub struct MeteoraListener {
    geyser_config: GeyserConfig,
    helius_ws_config: HeliusWsConfig,
    signal_source: String,
    signal_validator: Arc<SignalValidator>,
}

impl Default for MeteoraListener {
    fn default() -> Self {
        Self::new()
    }
}

impl MeteoraListener {
    pub fn new() -> Self {
        let signal_source = std::env::var("SIGNAL_SOURCE")
            .unwrap_or_else(|_| "geyser_grpc".to_string());
        
        Self {
            geyser_config: GeyserConfig::default(),
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: Arc::new(SignalValidator::with_default()),
        }
    }

    pub fn with_geyser_config(config: GeyserConfig) -> Self {
        let signal_source = std::env::var("SIGNAL_SOURCE")
            .unwrap_or_else(|_| "geyser_grpc".to_string());
        
        Self {
            geyser_config: config,
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: Arc::new(SignalValidator::with_default()),
        }
    }

    pub fn with_validator(config: GeyserConfig, validator: Arc<SignalValidator>) -> Self {
        let signal_source = std::env::var("SIGNAL_SOURCE")
            .unwrap_or_else(|_| "geyser_grpc".to_string());
        
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

    pub async fn subscribe_to_pool_creation(
        &self,
    ) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(100);
        let geyser_config = self.geyser_config.clone();
        let helius_ws_config = self.helius_ws_config.clone();
        let signal_source = self.signal_source.clone();

        // Collect all Meteora program IDs
        let program_ids = vec![
            Pubkey::from_str(METEORA_DLMM_PROGRAM)?,
            Pubkey::from_str(METEORA_DYNAMIC_PROGRAM)?,
        ];

        let validator = self.signal_validator.clone();

        tokio::spawn(async move {
            let mut account_updates = if signal_source == "helius_ws" {
                info!("Starting Meteora pool creation listener via Helius WebSocket");
                let helius_client = HeliusWsClient::new(helius_ws_config);
                helius_client.subscribe_to_programs(program_ids).await
                    .context("Failed to subscribe to Helius WebSocket")
                    .unwrap()
            } else {
                info!("Starting Meteora pool creation listener via Geyser gRPC");
                let geyser_client = GeyserClient::new(geyser_config);
                geyser_client.subscribe_to_programs(program_ids, None, false).await
                    .context("Failed to subscribe to Geyser gRPC")
                    .unwrap()
            };

            while let Some(update) = account_updates.recv().await {
                // Update current slot from account update
                validator.update_current_slot(update.slot);

                if let Some(signal) = Self::parse_meteora_account(update) {
                    // Validate signal age
                    match validator.validate_signal(&signal) {
                        Ok(true) => {
                            if tx.send(signal).await.is_err() {
                                info!("Receiver dropped, closing Meteora listener loop");
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

    /// Parse Meteora account update to extract pool creation signal.
    fn parse_meteora_account(update: AccountUpdate) -> Option<Signal> {
        let signal_source = std::env::var("SIGNAL_SOURCE")
            .unwrap_or_else(|_| "geyser_grpc".to_string());
        let account_data = update.data;
        let owner_str = update.owner.to_string();

        // Meteora DLMM LB Pair minimum length
        if account_data.len() >= 8 + 32 * 6 && owner_str == METEORA_DLMM_PROGRAM {
            let offset = 8 + 32 * 2; // token_x_mint offset
            if let Ok(mint_x) = read_pubkey_bytes(&account_data, offset) {
                return Some(Signal {
                    mint: mint_x.to_string(),
                    pool_address: update.account.to_string(),
                    dex_program: update.owner.to_string(),
                    signal_source: signal_source.clone(),
                    slot: update.slot,
                    received_at: Utc::now().timestamp_millis() as u64,
                });
            }
        }

        // Meteora Dynamic AMM pool account
        // Layout: discriminator(8) + token_a_mint(32)
        if account_data.len() >= 40 && owner_str == METEORA_DYNAMIC_PROGRAM {
            if let Ok(mint_a) = read_pubkey_bytes(&account_data, 8) {
                return Some(Signal {
                    mint: mint_a.to_string(),
                    pool_address: update.account.to_string(),
                    dex_program: update.owner.to_string(),
                    signal_source: signal_source.clone(),
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
