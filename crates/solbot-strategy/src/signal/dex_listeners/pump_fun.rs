//! Pump.fun bonding curve and migration listener.
//!
//! Subscribes to Pump.fun program account updates via Geyser gRPC
//! to detect new token launches and Raydium migration events.

use solbot_common::types::Signal;
use crate::signal::geyser_client::{GeyserClient, GeyserConfig, AccountUpdate};
use anyhow::{Context, Result};
use chrono::Utc;
use solana_pubkey::Pubkey;
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::{info};

pub const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2";

pub struct PumpFunListener {
    geyser_config: GeyserConfig,
}

impl Default for PumpFunListener {
    fn default() -> Self {
        Self::new()
    }
}

impl PumpFunListener {
    pub fn new() -> Self {
        Self {
            geyser_config: GeyserConfig::default(),
        }
    }

    pub fn with_geyser_config(config: GeyserConfig) -> Self {
        Self {
            geyser_config: config,
        }
    }

    /// Subscribe to new bonding curve creation events.
    pub async fn subscribe_to_bonding_curve(
        &self,
    ) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(100);
        let geyser_config = self.geyser_config.clone();

        let program_ids = vec![Pubkey::from_str(PUMP_FUN_PROGRAM)?];

        let geyser_client = GeyserClient::new(geyser_config);
        let mut account_updates = geyser_client
            .subscribe_to_programs(program_ids, None, false)
            .await
            .context("Failed to subscribe to Geyser gRPC")?;

        tokio::spawn(async move {
            info!("Starting Pump.fun bonding curve listener via Geyser gRPC");

            while let Some(update) = account_updates.recv().await {
                if let Some(signal) = Self::parse_pump_fun_account(update) {
                    if tx.send(signal).await.is_err() {
                        info!("Receiver dropped, closing Pump.fun listener loop");
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Parse Pump.fun account update to extract bonding curve signal.
    fn parse_pump_fun_account(update: AccountUpdate) -> Option<Signal> {
        let account_data = update.data;

        // Pump.fun bonding curve data starts with discriminator + mint pubkey (32 bytes)
        if account_data.len() > 8 + 32 + 8 * 5 {
            if let Ok(mint) = read_pubkey_bytes(&account_data, 8) {
                return Some(Signal {
                    mint: mint.to_string(),
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
