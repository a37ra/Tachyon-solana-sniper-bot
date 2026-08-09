//! Meteora pool creation listener (DLMM LbPair, Dynamic AMM).
//!
//! Strict size bands reject bin-array account spam (~10kB) from programSubscribe.

use crate::signal::geyser_client::{AccountUpdate, GeyserClient, GeyserConfig};
use crate::signal::helius_ws_client::{HeliusWsClient, HeliusWsConfig};
use crate::signal::pool_signal::{self, SeenPools};
use crate::signal::signal_validator::SignalValidator;
use anyhow::{Context, Result};
use solana_pubkey::Pubkey;
use solbot_common::types::Signal;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Mainnet DLMM program (matches live WS owner logs).
pub const METEORA_DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
pub const METEORA_DYNAMIC_PROGRAM: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";

pub struct MeteoraListener {
    geyser_config: GeyserConfig,
    helius_ws_config: HeliusWsConfig,
    signal_source: String,
    signal_validator: Arc<SignalValidator>,
    seen_pools: SeenPools,
}

impl Default for MeteoraListener {
    fn default() -> Self {
        Self::new()
    }
}

impl MeteoraListener {
    pub fn new() -> Self {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "helius_ws".to_string());
        Self {
            geyser_config: GeyserConfig::default(),
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: Arc::new(SignalValidator::with_default()),
            seen_pools: SeenPools::with_warmup_secs(0),
        }
    }

    pub fn with_geyser_config(config: GeyserConfig) -> Self {
        Self {
            geyser_config: config,
            ..Self::new()
        }
    }

    pub fn with_validator(config: GeyserConfig, validator: Arc<SignalValidator>) -> Self {
        Self {
            geyser_config: config,
            signal_validator: validator,
            ..Self::new()
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
            seen_pools: SeenPools::with_warmup_secs(0),
        }
    }

    pub async fn subscribe_to_pool_creation(&self) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(256);
        let geyser_config = self.geyser_config.clone();
        let helius_ws_config = self.helius_ws_config.clone();
        let signal_source = self.signal_source.clone();
        let seen = self.seen_pools.clone();

        let program_ids = vec![
            Pubkey::from_str(METEORA_DLMM_PROGRAM)?,
            Pubkey::from_str(METEORA_DYNAMIC_PROGRAM)?,
        ];

        let validator = self.signal_validator.clone();

        tokio::spawn(async move {
            let mut account_updates = if signal_source == "helius_ws" {
                info!("Starting Meteora pool listener (strict filter) via Helius WS");
                let helius_client = HeliusWsClient::new(helius_ws_config);
                helius_client
                    .subscribe_to_programs(program_ids)
                    .await
                    .context("Failed to subscribe to Helius WebSocket")
                    .unwrap()
            } else {
                info!("Starting Meteora pool listener (strict filter) via Geyser gRPC");
                let geyser_client = GeyserClient::new(geyser_config);
                geyser_client
                    .subscribe_to_programs(program_ids, None, false)
                    .await
                    .context("Failed to subscribe to Geyser gRPC")
                    .unwrap()
            };

            while let Some(update) = account_updates.recv().await {
                validator.update_current_slot(update.slot);
                if let Some(signal) = parse_meteora_account(update, &signal_source, &seen) {
                    match validator.validate_signal(&signal) {
                        Ok(true) => {
                            if tx.send(signal).await.is_err() {
                                break;
                            }
                        }
                        Ok(false) => warn!("Meteora signal filtered (slot age)"),
                        Err(e) => warn!(error = %e, "Meteora validation error"),
                    }
                }
            }
        });

        Ok(rx)
    }
}

pub fn parse_meteora_account(
    update: AccountUpdate,
    signal_source: &str,
    seen: &SeenPools,
) -> Option<Signal> {
    let owner = update.owner;
    let owner_str = owner.to_string();
    let data = &update.data;

    if owner_str == METEORA_DLMM_PROGRAM {
        return pool_signal::try_parse_meteora_dlmm(
            update.account,
            owner,
            data,
            update.slot,
            update.timestamp_ms,
            signal_source,
            seen,
        );
    }
    if owner_str == METEORA_DYNAMIC_PROGRAM {
        // cpamdp... is DAMM v2 (not legacy Dynamic AMM Eo7WjKq...).
        return pool_signal::try_parse_meteora_damm_v2(
            update.account,
            owner,
            data,
            update.slot,
            update.timestamp_ms,
            signal_source,
            seen,
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::geyser_client::AccountUpdate;

    #[test]
    fn bin_array_noise_does_not_emit() {
        let seen = SeenPools::with_warmup_secs(0);
        let u = AccountUpdate {
            account: Pubkey::new_unique(),
            slot: 1,
            lamports: 0,
            data: vec![0u8; 10136],
            owner: Pubkey::from_str(METEORA_DLMM_PROGRAM).unwrap(),
            executable: false,
            rent_epoch: 0,
            timestamp_ms: 1,
        };
        assert!(parse_meteora_account(u, "test", &seen).is_none());
    }
}
