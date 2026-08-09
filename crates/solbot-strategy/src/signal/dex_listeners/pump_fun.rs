//! Pump.fun bonding curve listener.
//!
//! Only emits on **first sighting** of a bonding-curve PDA (not every trade update).

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

pub const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

pub struct PumpFunListener {
    geyser_config: GeyserConfig,
    helius_ws_config: HeliusWsConfig,
    signal_source: String,
    signal_validator: Arc<SignalValidator>,
    seen_curves: SeenPools,
}

impl Default for PumpFunListener {
    fn default() -> Self {
        Self::new()
    }
}

impl PumpFunListener {
    pub fn new() -> Self {
        let signal_source =
            std::env::var("SIGNAL_SOURCE").unwrap_or_else(|_| "helius_ws".to_string());
        Self {
            geyser_config: GeyserConfig::default(),
            helius_ws_config: HeliusWsConfig::default(),
            signal_source,
            signal_validator: Arc::new(SignalValidator::with_default()),
            seen_curves: SeenPools::with_warmup_secs(0),
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
            seen_curves: SeenPools::with_warmup_secs(0),
        }
    }

    pub async fn subscribe_to_bonding_curve(&self) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(256);
        let geyser_config = self.geyser_config.clone();
        let helius_ws_config = self.helius_ws_config.clone();
        let signal_source = self.signal_source.clone();
        let seen = self.seen_curves.clone();

        let program_ids = vec![Pubkey::from_str(PUMP_FUN_PROGRAM)?];
        let validator = self.signal_validator.clone();

        tokio::spawn(async move {
            let mut account_updates = if signal_source == "helius_ws" {
                info!("Starting Pump.fun listener (PDA + first-seen) via Helius WS");
                let helius_client = HeliusWsClient::new(helius_ws_config);
                helius_client
                    .subscribe_to_programs(program_ids)
                    .await
                    .context("Failed to subscribe to Helius WebSocket")
                    .unwrap()
            } else {
                info!("Starting Pump.fun listener (PDA + first-seen) via Geyser gRPC");
                let geyser_client = GeyserClient::new(geyser_config);
                geyser_client
                    .subscribe_to_programs(program_ids, None, false)
                    .await
                    .context("Failed to subscribe to Geyser gRPC")
                    .unwrap()
            };

            while let Some(update) = account_updates.recv().await {
                validator.update_current_slot(update.slot);
                // Active curves (new launch)
                if let Some(signal) =
                    parse_pump_fun_account(update.clone(), &signal_source, &seen, false)
                {
                    match validator.validate_signal(&signal) {
                        Ok(true) => {
                            if tx.send(signal).await.is_err() {
                                break;
                            }
                        }
                        Ok(false) => warn!("Pump signal filtered (slot age)"),
                        Err(e) => warn!(error = %e, "Pump validation error"),
                    }
                }
                // Migration events (complete=true) — separate claim key handled inside parser
                // via same seen set on same account address (first complete sighting only if
                // we never saw the curve before; if we saw as active, migration is skipped —
                // migration is handled when complete flips on first observation of complete).
                if let Some(signal) =
                    parse_pump_fun_account(update, "pump_fun_migration", &seen, true)
                {
                    match validator.validate_signal(&signal) {
                        Ok(true) => {
                            let _ = tx.send(signal).await;
                        }
                        _ => {}
                    }
                }
            }
        });

        Ok(rx)
    }
}

pub fn parse_pump_fun_account(
    update: AccountUpdate,
    signal_source: &str,
    seen: &SeenPools,
    migration_only: bool,
) -> Option<Signal> {
    pool_signal::try_parse_pump_bonding_curve(
        update.account,
        update.owner,
        &update.data,
        update.slot,
        update.timestamp_ms,
        signal_source,
        seen,
        migration_only,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::geyser_client::AccountUpdate;

    #[test]
    fn system_program_mint_noise_no_signal() {
        let seen = SeenPools::with_warmup_secs(0);
        let mut data = vec![0u8; 90];
        // mint bytes = system program
        let sys = Pubkey::from_str("11111111111111111111111111111111").unwrap();
        data[8..40].copy_from_slice(sys.as_ref());
        let owner = Pubkey::from_str(PUMP_FUN_PROGRAM).unwrap();
        let (pda, _) = Pubkey::find_program_address(&[b"bonding-curve", sys.as_ref()], &owner);
        let u = AccountUpdate {
            account: pda,
            slot: 1,
            lamports: 0,
            data,
            owner,
            executable: false,
            rent_epoch: 0,
            timestamp_ms: 1,
        };
        assert!(parse_pump_fun_account(u, "test", &seen, false).is_none());
    }
}
