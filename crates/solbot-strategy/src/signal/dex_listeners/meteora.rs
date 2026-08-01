//! Meteora pool creation listener (DLMM, Dynamic AMM).
//!
//! Subscribes to Meteora DLMM / Dynamic AMM program accounts via Geyser gRPC
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

pub const METEORA_DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp";
pub const METEORA_DYNAMIC_PROGRAM: &str = "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB";

pub struct MeteoraListener {
    geyser_config: GeyserConfig,
    signal_validator: Arc<SignalValidator>,
}

impl Default for MeteoraListener {
    fn default() -> Self {
        Self::new()
    }
}

impl MeteoraListener {
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

    pub async fn subscribe_to_pool_creation(
        &self,
    ) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(100);
        let geyser_config = self.geyser_config.clone();

        // Collect all Meteora program IDs
        let program_ids = vec![
            Pubkey::from_str(METEORA_DLMM_PROGRAM)?,
            Pubkey::from_str(METEORA_DYNAMIC_PROGRAM)?,
        ];

        let geyser_client = GeyserClient::new(geyser_config);
        let mut account_updates = geyser_client
            .subscribe_to_programs(program_ids, None, false)
            .await
            .context("Failed to subscribe to Geyser gRPC")?;

        let validator = self.signal_validator.clone();

        tokio::spawn(async move {
            info!("Starting Meteora pool creation listener via Geyser gRPC");

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
                    signal_source: "geyser_grpc".to_string(),
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
