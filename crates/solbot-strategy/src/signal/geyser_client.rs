//! Geyser gRPC client for real-time Solana account updates.
//!
//! Provides a unified interface for subscribing to account updates via
//! Yellowstone/Geyser gRPC protocol with filtering by program_id and
//! automatic reconnection with exponential backoff.

use anyhow::Result;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};
use tracing::{info, warn, error};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::*;
use std::collections::HashMap;

/// Configuration for Geyser gRPC subscription.
#[derive(Debug, Clone)]
pub struct GeyserConfig {
    /// gRPC endpoint URL (e.g., "http://localhost:10000" or Helius endpoint)
    pub endpoint: String,
    /// Optional authentication token
    pub x_token: Option<String>,
    /// Commitment level for subscription
    pub commitment: i32,
    /// RPC URL for backfill (optional)
    pub backfill_rpc_url: Option<String>,
    /// Maximum slot age for backfill (slots)
    pub max_backfill_slots: u64,
}

impl Default for GeyserConfig {
    fn default() -> Self {
        Self {
            endpoint: std::env::var("GEYSER_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:10000".to_string()),
            x_token: std::env::var("GEYSER_X_TOKEN").ok(),
            commitment: 2, // Confirmed
            backfill_rpc_url: std::env::var("BACKFILL_RPC_URL").ok(),
            max_backfill_slots: 150, // Default: backfill last 150 slots
        }
    }
}

/// DEX program IDs for filtering
pub mod dex_programs {
    use super::Pubkey;
    use std::str::FromStr;

    /// Raydium AMM v4
    pub const RAYDIUM_AMM_V4: Pubkey = solana_pubkey::pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
    /// Meteora DLMM
    pub const METEORA_DLMM: Pubkey = solana_pubkey::pubkey!("LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp");
    /// Pump.fun bonding curve
    pub const PUMP_FUN: Pubkey = solana_pubkey::pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2");

    /// Raydium CP-MM (placeholder - needs real program ID)
    pub fn raydium_cp_mm() -> Pubkey {
        Pubkey::from_str("CPMMoo8L38FZJd9rBjKLYx9nP5BHDK2oD9WcM9hCqN").unwrap()
    }
    /// Raydium CLMM (placeholder - needs real program ID)
    pub fn raydium_clmm() -> Pubkey {
        Pubkey::from_str("CLMMx9s3DmDKhVYRkb7ZMokjWnNLYMG2fCJ5xZ7Wj2").unwrap()
    }
    /// Meteora Dynamic AMM (placeholder - needs real program ID)
    pub fn meteora_dynamic_amm() -> Pubkey {
        Pubkey::from_str("DyWWnUfSxvHMjy5yv5MJKXfJLgZ2m5gZxZxZxZxZxZx").unwrap()
    }
}

/// Parsed account update notification.
#[derive(Debug, Clone)]
pub struct AccountUpdate {
    pub account: Pubkey,
    pub slot: u64,
    pub lamports: u64,
    pub data: Vec<u8>,
    pub owner: Pubkey,
    pub executable: bool,
    pub rent_epoch: u64,
}

/// Extended history provider for backfilling missed slots.
pub struct ExtendedHistoryProvider {
    rpc_url: String,
}

impl ExtendedHistoryProvider {
    pub fn new(rpc_url: String) -> Self {
        Self { rpc_url }
    }

    /// Backfill account updates for missed slots.
    ///
    /// Fetches account changes for the given program IDs between the specified slots.
    pub async fn backfill_slots(
        &self,
        _program_ids: &[Pubkey],
        _start_slot: u64,
        _end_slot: u64,
    ) -> Result<Vec<AccountUpdate>> {
        // TODO: Implement actual backfill logic using RPC getAccountChanges
        // This would typically use getProgramAccounts with filters or
        // a specialized extended history API if available
        tracing::warn!("Extended history backfill not yet implemented");
        Ok(vec![])
    }
}

/// Geyser gRPC client with automatic reconnection.
pub struct GeyserClient {
    config: GeyserConfig,
    last_seen_slot: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl GeyserClient {
    pub fn new(config: GeyserConfig) -> Self {
        Self {
            config,
            last_seen_slot: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn from_env() -> Self {
        Self::new(GeyserConfig::default())
    }

    /// Get the last seen slot for backfill purposes.
    pub fn get_last_seen_slot(&self) -> u64 {
        self.last_seen_slot.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Perform backfill for missed slots if configured.
    async fn perform_backfill(
        &self,
        program_ids: &[Pubkey],
        tx: &mpsc::Sender<AccountUpdate>,
    ) {
        if let Some(ref rpc_url) = self.config.backfill_rpc_url {
            let last_slot = self.get_last_seen_slot();
            if last_slot > 0 {
                let provider = ExtendedHistoryProvider::new(rpc_url.clone());
                // TODO: Get current slot from RPC to calculate missed range
                let current_slot = last_slot; // Placeholder
                let missed_slots = current_slot.saturating_sub(last_slot);

                if missed_slots > 0 && missed_slots <= self.config.max_backfill_slots {
                    info!(
                        last_slot = last_slot,
                        current_slot = current_slot,
                        missed_slots = missed_slots,
                        "Performing backfill for missed slots"
                    );

                    match provider
                        .backfill_slots(program_ids, last_slot, current_slot)
                        .await
                    {
                        Ok(updates) => {
                            info!(count = updates.len(), "Backfilled account updates");
                            for update in updates {
                                if tx.send(update).await.is_err() {
                                    warn!("Receiver dropped during backfill");
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Backfill failed");
                        }
                    }
                }
            }
        }
    }

    /// Subscribe to account updates for specific program IDs.
    ///
    /// Returns a channel that receives parsed account updates.
    /// The subscription includes automatic reconnection with exponential backoff.
    pub async fn subscribe_to_programs(
        &self,
        program_ids: Vec<Pubkey>,
        _accounts_include: Option<Vec<Pubkey>>,
        _vote: bool,
    ) -> Result<mpsc::Receiver<AccountUpdate>> {
        let (tx, rx) = mpsc::channel(1000);
        let config = self.config.clone();
        let last_seen_slot = self.last_seen_slot.clone();

        tokio::spawn(async move {
            let mut reconnect_attempts = 0;
            let max_reconnect_attempts = 10;
            let base_backoff = Duration::from_secs(1);
            let max_backoff = Duration::from_secs(60);

            loop {
                info!(
                    endpoint = %config.endpoint,
                    attempt = reconnect_attempts,
                    "Connecting to Geyser gRPC"
                );

                // Create client with tonic 0.12
                let mut client_builder = match GeyserGrpcClient::build_from_shared(config.endpoint.clone()) {
                    Ok(builder) => builder,
                    Err(e) => {
                        error!(error = %e, "Failed to parse Geyser endpoint");
                        if reconnect_attempts >= max_reconnect_attempts {
                            error!("Max reconnect attempts reached, giving up");
                            break;
                        }
                        let backoff = (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                        warn!("Reconnecting in {:?}...", backoff);
                        sleep(backoff).await;
                        reconnect_attempts += 1;
                        continue;
                    }
                };

                client_builder = match client_builder.tls_config(Default::default()) {
                    Ok(builder) => builder,
                    Err(e) => {
                        warn!(error = %e, "Failed to set TLS config, continuing without");
                        return;
                    }
                };

                if let Some(token) = config.x_token.clone() {
                    client_builder = match client_builder.x_token(Some(token)) {
                        Ok(builder) => builder,
                        Err(e) => {
                            warn!(error = %e, "Failed to set x_token, continuing without");
                            return;
                        }
                    };
                }

                let mut client = match client_builder.connect().await {
                    Ok(c) => c,
                    Err(e) => {
                        error!(error = %e, "Failed to connect to Geyser gRPC");
                        if reconnect_attempts >= max_reconnect_attempts {
                            error!("Max reconnect attempts reached, giving up");
                            break;
                        }
                        let backoff = (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                        warn!("Reconnecting in {:?}...", backoff);
                        sleep(backoff).await;
                        reconnect_attempts += 1;
                        continue;
                    }
                };

                info!("Connected to Geyser gRPC");
                reconnect_attempts = 0; // Reset on successful connection

                // Perform backfill if this is a reconnection
                if reconnect_attempts > 0 {
                    // TODO: Implement actual backfill logic
                    info!("Skipping backfill (not yet fully implemented)");
                }

                // Build subscription request
                let mut accounts_map = HashMap::new();
                for program_id in &program_ids {
                    accounts_map.insert(
                        program_id.to_string(),
                        SubscribeRequestFilterAccounts {
                            owner: vec![program_id.to_string()],
                            account: vec![],
                            filters: vec![],
                            nonempty_txn_signature: Some(false),
                        },
                    );
                }

                let mut transactions_map = HashMap::new();
                transactions_map.insert(
                    "all".to_string(),
                    SubscribeRequestFilterTransactions {
                        vote: Some(false),
                        failed: Some(false),
                        ..Default::default()
                    },
                );

                let subscribe_request = SubscribeRequest {
                    accounts: accounts_map,
                    transactions: transactions_map,
                    commitment: Some(config.commitment),
                    ..Default::default()
                };

                let mut stream = match client.subscribe_once(subscribe_request).await {
                    Ok(s) => s,
                    Err(e) => {
                        error!(error = %e, "Failed to subscribe to Geyser gRPC");
                        if reconnect_attempts >= max_reconnect_attempts {
                            error!("Max reconnect attempts reached, giving up");
                            break;
                        }
                        let backoff = (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                        warn!("Reconnecting in {:?}...", backoff);
                        sleep(backoff).await;
                        reconnect_attempts += 1;
                        continue;
                    }
                };

                info!("Subscribed to Geyser gRPC");

                use tokio_stream::StreamExt;
                loop {
                    match stream.next().await {
                        Some(Ok(update)) => {
                            if let Some(update_oneof) = update.update_oneof {
                                if let yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof::Account(account_update) = update_oneof {
                                    if let Some(account) = account_update.account {
                                        let pubkey = match Pubkey::try_from(account.pubkey.as_slice()) {
                                            Ok(pk) => pk,
                                            Err(e) => {
                                                warn!(error = %e, "Failed to parse pubkey from account update");
                                                continue;
                                            }
                                        };
                                        let owner = match Pubkey::try_from(account.owner.as_slice()) {
                                            Ok(pk) => pk,
                                            Err(e) => {
                                                warn!(error = %e, "Failed to parse owner from account update");
                                                continue;
                                            }
                                        };

                                        let account_update = AccountUpdate {
                                            account: pubkey,
                                            slot: account_update.slot,
                                            lamports: account.lamports,
                                            data: account.data,
                                            owner,
                                            executable: account.executable,
                                            rent_epoch: account.rent_epoch,
                                        };

                                        // Update last seen slot
                                        last_seen_slot.store(
                                            account_update.slot,
                                            std::sync::atomic::Ordering::SeqCst,
                                        );

                                        if tx.send(account_update).await.is_err() {
                                            info!("Receiver dropped, closing Geyser listener loop");
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            error!(error = %e, "Geyser stream error, reconnecting...");
                            break; // Break inner loop to reconnect
                        }
                        None => {
                            warn!("Geyser stream closed, reconnecting...");
                            break; // Break inner loop to reconnect
                        }
                    }
                }
            }
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_geyser_config_default() {
        let config = GeyserConfig::default();
        assert!(!config.endpoint.is_empty());
    }
}
