//! Raydium pool creation listener (AMM v4, CP-MM, CLMM).
//!
//! Subscribes to Raydium program account updates via Geyser gRPC
//! with automatic reconnection and exponential backoff.

use solbot_common::types::Signal;
use crate::signal::geyser_client::{GeyserClient, GeyserConfig, AccountUpdate};
use anyhow::{Context, Result};
use chrono::Utc;
use solana_pubkey::Pubkey;
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::{info};

pub const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_CPMM_PROGRAM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
pub const RAYDIUM_CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";

pub struct RaydiumListener {
    geyser_config: GeyserConfig,
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
        }
    }

    pub fn with_geyser_config(config: GeyserConfig) -> Self {
        Self {
            geyser_config: config,
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

        tokio::spawn(async move {
            info!("Starting Raydium pool creation listener via Geyser gRPC");

            while let Some(update) = account_updates.recv().await {
                if let Some(signal) = Self::parse_raydium_account(update) {
                    if tx.send(signal).await.is_err() {
                        info!("Receiver dropped, closing Raydium listener loop");
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Parse Raydium account update to extract pool creation signal.
    fn parse_raydium_account(update: AccountUpdate) -> Option<Signal> {
        let account_data = update.data;

        // Raydium AMM v4 pool account size is 752 bytes
        if account_data.len() >= 752 {
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

        // Raydium CP-MM pool account (different structure)
        // Add CP-MM parsing logic here when needed

        // Raydium CLMM pool account (different structure)
        // Add CLMM parsing logic here when needed

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
