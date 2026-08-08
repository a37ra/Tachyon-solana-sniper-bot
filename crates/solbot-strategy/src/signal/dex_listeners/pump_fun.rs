//! Pump.fun bonding curve and migration listener.
//!
//! Subscribes to Pump.fun program account updates via Geyser gRPC
//! to detect new token launches and Raydium migration events.

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

pub const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

pub struct PumpFunListener {
    geyser_config: GeyserConfig,
    helius_ws_config: HeliusWsConfig,
    signal_source: String,
    signal_validator: Arc<SignalValidator>,
}

impl Default for PumpFunListener {
    fn default() -> Self {
        Self::new()
    }
}

impl PumpFunListener {
    pub fn new() -> Self {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "geyser_grpc".to_string());

        Self {
            geyser_config: GeyserConfig::default(),
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: Arc::new(SignalValidator::with_default()),
        }
    }

    pub fn with_geyser_config(config: GeyserConfig) -> Self {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "geyser_grpc".to_string());

        Self {
            geyser_config: config,
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: Arc::new(SignalValidator::with_default()),
        }
    }

    pub fn with_validator(config: GeyserConfig, validator: Arc<SignalValidator>) -> Self {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "geyser_grpc".to_string());

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

    /// Subscribe to new bonding curve creation events.
    pub async fn subscribe_to_bonding_curve(&self) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(100);
        let geyser_config = self.geyser_config.clone();
        let helius_ws_config = self.helius_ws_config.clone();
        let signal_source = self.signal_source.clone();

        let program_ids = vec![Pubkey::from_str(PUMP_FUN_PROGRAM)?];

        let validator = self.signal_validator.clone();

        tokio::spawn(async move {
            let mut account_updates = if signal_source == "helius_ws" {
                info!("Starting Pump.fun bonding curve listener via Helius WebSocket");
                let helius_client = HeliusWsClient::new(helius_ws_config);
                helius_client
                    .subscribe_to_programs(program_ids)
                    .await
                    .context("Failed to subscribe to Helius WebSocket")
                    .unwrap()
            } else {
                info!("Starting Pump.fun bonding curve listener via Geyser gRPC");
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

                if let Some(signal) = Self::parse_pump_fun_account(update) {
                    // Validate signal age
                    match validator.validate_signal(&signal) {
                        Ok(true) => {
                            if tx.send(signal).await.is_err() {
                                info!("Receiver dropped, closing Pump.fun listener loop");
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

    /// Parse Pump.fun account update to extract bonding curve signal.
    fn parse_pump_fun_account(update: AccountUpdate) -> Option<Signal> {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "geyser_grpc".to_string());
        let account_data = update.data;

        // Pump.fun bonding curve data structure:
        // discriminator(8) + mint(32) + reserves(8*5) + complete(1) = 8 + 32 + 40 + 1 = 81 bytes minimum
        if account_data.len() > 72 {
            // Check for migration event (complete field at offset 72)
            let is_complete = account_data[72] != 0;

            if is_complete {
                // Token has migrated to Raydium!
                // Extract mint to track the Raydium pool
                    if let Ok(mint) = read_pubkey_bytes(&account_data, 8) {
                    return Some(Signal {
                        mint: mint.to_string(),
                        pool_address: update.account.to_string(),
                        dex_program: update.owner.to_string(),
                        signal_source: "pump_fun_migration".to_string(),
                        slot: update.slot,
                        received_at: update.timestamp_ms,
                        raw_pool_account_data: Some(account_data.clone()),
                        raw_pool_account_owner: Some(update.owner.to_string()),
                    });
                }
            }

            // New bonding curve creation (complete == false)
            if !is_complete && account_data.len() > 8 + 32 + 8 * 5 {
                if let Ok(mint) = read_pubkey_bytes(&account_data, 8) {
                    return Some(Signal {
                        mint: mint.to_string(),
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
