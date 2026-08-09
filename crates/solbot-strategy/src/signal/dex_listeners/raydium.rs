//! Raydium pool creation listener (AMM v4, CP-MM, CLMM).
//!
//! Subscribes via programSubscribe / Geyser, then applies **strict** pool-state
//! filters (size + mint + first-seen) so swap/tick-array updates never become signals.

use crate::signal::geyser_client::{AccountUpdate, GeyserClient, GeyserConfig};
use crate::signal::helius_ws_client::{HeliusWsClient, HeliusWsConfig};
use crate::signal::pool_signal::{
    self, SeenPools, RAYDIUM_AMM_V4_SIZE, RAYDIUM_CLMM_POOL_SIZE, RAYDIUM_CPMM_SIZE_MAX,
    RAYDIUM_CPMM_SIZE_MIN,
};
use crate::signal::signal_validator::SignalValidator;
use anyhow::{Context, Result};
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
    seen_pools: SeenPools,
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
            Pubkey::from_str(RAYDIUM_AMM_V4_PROGRAM)?,
            Pubkey::from_str(RAYDIUM_CPMM_PROGRAM)?,
            Pubkey::from_str(RAYDIUM_CLMM_PROGRAM)?,
        ];

        let validator = self.signal_validator.clone();

        tokio::spawn(async move {
            let mut account_updates = if signal_source == "helius_ws" {
                info!("Starting Raydium pool listener (strict filter) via Helius WS");
                let helius_client = HeliusWsClient::new(helius_ws_config);
                helius_client
                    .subscribe_to_programs(program_ids)
                    .await
                    .context("Failed to subscribe to Helius WebSocket")
                    .unwrap()
            } else {
                info!("Starting Raydium pool listener (strict filter) via Geyser gRPC");
                let geyser_client = GeyserClient::new(geyser_config);
                geyser_client
                    .subscribe_to_programs(program_ids, None, false)
                    .await
                    .context("Failed to subscribe to Geyser gRPC")
                    .unwrap()
            };

            while let Some(update) = account_updates.recv().await {
                validator.update_current_slot(update.slot);

                if let Some(signal) = parse_raydium_account(update, &signal_source, &seen) {
                    match validator.validate_signal(&signal) {
                        Ok(true) => {
                            if tx.send(signal).await.is_err() {
                                info!("Receiver dropped, closing Raydium listener");
                                break;
                            }
                        }
                        Ok(false) => {
                            warn!(mint = %signal.mint, "Raydium signal filtered (slot age)");
                        }
                        Err(e) => warn!(error = %e, "Raydium signal validation error"),
                    }
                }
            }
        });

        Ok(rx)
    }
}

/// Public for unit tests — full "is this a new Raydium pool?" decision.
pub fn parse_raydium_account(
    update: AccountUpdate,
    signal_source: &str,
    seen: &SeenPools,
) -> Option<Signal> {
    let owner = update.owner;
    let owner_str = owner.to_string();
    let data = &update.data;

    if owner_str == RAYDIUM_AMM_V4_PROGRAM {
        return pool_signal::try_parse_raydium_amm_v4(
            update.account,
            owner,
            data,
            update.slot,
            update.timestamp_ms,
            signal_source,
            seen,
        );
    }
    if owner_str == RAYDIUM_CPMM_PROGRAM {
        return pool_signal::try_parse_raydium_cpmm(
            update.account,
            owner,
            data,
            update.slot,
            update.timestamp_ms,
            signal_source,
            seen,
        );
    }
    if owner_str == RAYDIUM_CLMM_PROGRAM {
        return pool_signal::try_parse_raydium_clmm(
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

    fn upd(owner: &str, data: Vec<u8>) -> AccountUpdate {
        AccountUpdate {
            account: Pubkey::new_unique(),
            slot: 1,
            lamports: 0,
            data,
            owner: Pubkey::from_str(owner).unwrap(),
            executable: false,
            rent_epoch: 0,
            timestamp_ms: 1,
        }
    }

    #[test]
    fn garbage_clmm_tick_array_does_not_emit_signal() {
        let seen = SeenPools::with_warmup_secs(0);
        let u = upd(RAYDIUM_CLMM_PROGRAM, vec![0u8; 10240]);
        assert!(parse_raydium_account(u, "test", &seen).is_none());
    }

    #[test]
    fn wrong_size_amm_v4_no_signal() {
        let seen = SeenPools::with_warmup_secs(0);
        let u = upd(RAYDIUM_AMM_V4_PROGRAM, vec![0u8; 100]);
        assert!(parse_raydium_account(u, "test", &seen).is_none());
        assert_eq!(RAYDIUM_AMM_V4_SIZE, 752);
        let _ = (RAYDIUM_CLMM_POOL_SIZE, RAYDIUM_CPMM_SIZE_MIN, RAYDIUM_CPMM_SIZE_MAX);
    }
}
