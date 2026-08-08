//! Geyser gRPC client for real-time Solana account updates.
//!
//! Provides a unified interface for subscribing to account updates via
//! Yellowstone/Geyser gRPC protocol with filtering by program_id and
//! automatic reconnection with exponential backoff.

use anyhow::{Context, Result};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout, Duration};
use tonic::transport::ClientTlsConfig;
use tracing::{error, info, warn};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::*;

/// Configuration for Geyser gRPC subscription.
#[derive(Debug, Clone)]
pub struct GeyserConfig {
    /// gRPC endpoint URL (e.g., "http://localhost:10000" or Helius endpoint)
    pub endpoint: String,
    /// Authentication token (Helius API key or Yellowstone x-token)
    pub x_token: Option<String>,
    /// Commitment level for subscription (0=processed, 1=confirmed, 2=finalized)
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
                .unwrap_or_else(|_| "https://mainnet.helius-rpc.com".to_string()),
            x_token: std::env::var("GEYSER_X_TOKEN").ok(),
            commitment: 2, // Confirmed
            backfill_rpc_url: std::env::var("BACKFILL_RPC_URL").ok(),
            max_backfill_slots: 150, // Default: backfill last 150 slots
        }
    }
}

impl GeyserConfig {
    /// Create GeyserConfig from BaseConfig (YAML)
    pub fn from_base_config(
        base_config: &solbot_common::config::base_config::GeyserConfig,
    ) -> Self {
        let commitment = match base_config.commitment.as_str() {
            "processed" => 0,
            "confirmed" => 1,
            "finalized" => 2,
            _ => 1, // Default to confirmed
        };

        Self {
            endpoint: base_config.endpoint.clone(),
            x_token: Some(base_config.x_token.clone()),
            commitment,
            backfill_rpc_url: Some(base_config.backfill_rpc_url.clone()),
            max_backfill_slots: base_config.max_backfill_slots,
        }
    }
}

/// DEX program IDs for filtering
pub mod dex_programs {
    use super::Pubkey;
    use std::str::FromStr;

    /// Raydium AMM v4
    pub const RAYDIUM_AMM_V4: Pubkey =
        solana_pubkey::pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
    /// Meteora DLMM
    pub const METEORA_DLMM: Pubkey =
        solana_pubkey::pubkey!("LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp");
    /// Pump.fun bonding curve
    pub const PUMP_FUN: Pubkey =
        solana_pubkey::pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2");

    /// Raydium CP-MM
    pub fn raydium_cp_mm() -> Pubkey {
        Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C").unwrap()
    }
    /// Raydium CLMM
    pub fn raydium_clmm() -> Pubkey {
        Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap()
    }
    /// Meteora Dynamic AMM v1
    pub fn meteora_dynamic_amm() -> Pubkey {
        Pubkey::from_str("Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB").unwrap()
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
    pub timestamp_ms: u64, // Unix timestamp in milliseconds when this update was received
}

/// Extended history provider for backfilling missed slots.
///
/// This provider checks open positions during gRPC reconnection to detect
/// rug pulls, migrations, or other critical state changes.
#[derive(Clone)]
pub struct ExtendedHistoryProvider {
    rpc: Arc<RpcClient>,
    db_pool: Arc<SqlitePool>,
}

impl ExtendedHistoryProvider {
    pub fn new(rpc: Arc<RpcClient>, db_pool: Arc<SqlitePool>) -> Self {
        Self { rpc, db_pool }
    }

    /// Backfill account updates for missed slots.
    ///
    /// Note: This method is kept for API compatibility but is not used.
    /// The actual backfill logic is in `perform_backfill()` which checks
    /// open positions instead of scanning slots.
    pub async fn backfill_slots(
        &self,
        _program_ids: &[Pubkey],
        _start_slot: u64,
        _end_slot: u64,
    ) -> Result<Vec<AccountUpdate>> {
        // Not used - we perform position-based backfill instead
        Ok(vec![])
    }

    /// Perform backfill for open positions after gRPC reconnection.
    ///
    /// This method:
    /// 1. Queries SQLite for all positions with status 'opening' or 'open'
    /// 2. For each position, fetches the pool account via RPC
    /// 3. Detects rug pulls (pool balance = 0) or Pump.fun migrations (complete = true)
    /// 4. Logs warnings for detected issues
    /// 5. Has a 150ms hard timeout to avoid blocking the main thread
    pub async fn perform_backfill(&self) -> Result<usize> {
        let backfill_start = std::time::Instant::now();

        let result = timeout(Duration::from_millis(150), self.perform_backfill_internal()).await;

        match result {
            Ok(Ok(count)) => {
                info!(
                    positions_checked = count,
                    duration_ms = backfill_start.elapsed().as_millis(),
                    "Backfill completed successfully"
                );
                Ok(count)
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Backfill internal error");
                Err(e)
            }
            Err(_) => {
                warn!("Backfill timeout after 150ms");
                Ok(0) // Return success with 0 positions checked
            }
        }
    }

    async fn perform_backfill_internal(&self) -> Result<usize> {
        // Query open positions from SQLite using runtime query instead of compile-time checked
        let positions = sqlx::query_as::<_, (i64, String, String, String, String)>(
            r#"
            SELECT position_id, mint, pool_address, dex_program_id, status
            FROM positions
            WHERE status IN ('opening', 'open')
            "#,
        )
        .fetch_all(&*self.db_pool)
        .await
        .context("Failed to fetch open positions from database")?;

        let mut checked_count = 0;

        for position in positions {
            checked_count += 1;

            let (position_id, _mint, pool_address, dex_program_id, _status) = position;

            let pool_pubkey = match Pubkey::try_from(pool_address.as_str()) {
                Ok(pk) => pk,
                Err(e) => {
                    warn!(
                        position_id = %position_id,
                        error = %e,
                        "Invalid pool address"
                    );
                    continue;
                }
            };

            // Fetch pool account via RPC
            let account_info = tokio::task::spawn_blocking({
                let rpc = Arc::clone(&self.rpc);
                move || {
                    rpc.get_account(&pool_pubkey)
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
                }
            })
            .await;

            match account_info {
                Ok(Ok(account)) => {
                    // Check for rug pull: pool account with zero lamports
                    if account.lamports == 0 {
                        warn!(
                            position_id = %position_id,
                            pool_address = %pool_address,
                            "Detected potential rug pull: pool has zero lamports"
                        );
                        // TODO: Send ControlEvent to trigger exit strategy
                        continue;
                    }

                    // Check for Pump.fun migration
                    if dex_program_id == "6EF8rrecthR5Dkzon8Nwu78hRvfX4LctQnUBjWdK8nN2" {
                        // Pump.fun bonding curve: check complete field at offset 72
                        if account.data.len() > 72 {
                            let is_complete = account.data[72] != 0;
                            if is_complete {
                                info!(
                                    position_id = %position_id,
                                    pool_address = %pool_address,
                                    "Pump.fun token has migrated to Raydium"
                                );
                                // TODO: Send MigrationEvent to event bus
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    warn!(
                        position_id = %position_id,
                        pool_address = %pool_address,
                        error = %e,
                        "RPC error fetching pool account"
                    );
                }
                Err(e) => {
                    warn!(
                        position_id = %position_id,
                        error = %e,
                        "Task join error"
                    );
                }
            }
        }

        Ok(checked_count)
    }
}

/// Geyser gRPC client with automatic reconnection.
pub struct GeyserClient {
    config: GeyserConfig,
    last_seen_slot: std::sync::Arc<std::sync::atomic::AtomicU64>,
    backfill_provider: Option<ExtendedHistoryProvider>,
}

impl GeyserClient {
    pub fn new(config: GeyserConfig) -> Self {
        Self {
            config,
            last_seen_slot: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            backfill_provider: None,
        }
    }

    /// Create GeyserClient with backfill support for position checking.
    pub fn with_backfill(
        config: GeyserConfig,
        rpc: Arc<RpcClient>,
        db_pool: Arc<SqlitePool>,
    ) -> Self {
        let backfill_provider = ExtendedHistoryProvider::new(rpc, db_pool);
        Self {
            config,
            last_seen_slot: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            backfill_provider: Some(backfill_provider),
        }
    }

    pub fn from_env() -> Self {
        Self::new(GeyserConfig::default())
    }

    /// Get the last seen slot for backfill purposes.
    pub fn get_last_seen_slot(&self) -> u64 {
        self.last_seen_slot
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Subscribe to account updates for specific program IDs.
    ///
    /// Returns a channel that receives parsed account updates.
    /// The subscription includes automatic reconnection with exponential backoff.
    ///
    /// # Arguments
    /// * `program_ids` - List of program IDs to filter account updates by owner
    /// * `accounts_include` - Optional list of specific accounts to include (for fine-grained filtering)
    /// * `vote` - Whether to include vote transactions in the subscription (default: false to save credits)
    pub async fn subscribe_to_programs(
        &self,
        program_ids: Vec<Pubkey>,
        _accounts_include: Option<Vec<Pubkey>>,
        _vote: bool,
    ) -> Result<mpsc::Receiver<AccountUpdate>> {
        let (tx, rx) = mpsc::channel(1000);
        let config = self.config.clone();
        let last_seen_slot = self.last_seen_slot.clone();
        let backfill_provider = self.backfill_provider.clone();

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
                let client_builder =
                    match GeyserGrpcClient::build_from_shared(config.endpoint.clone()) {
                        Ok(builder) => builder,
                        Err(e) => {
                            error!(error = %e, "Failed to parse Geyser endpoint");
                            if reconnect_attempts >= max_reconnect_attempts {
                                error!("Max reconnect attempts reached, giving up");
                                break;
                            }
                            let backoff =
                                (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                            warn!("Reconnecting in {:?}...", backoff);
                            sleep(backoff).await;
                            reconnect_attempts += 1;
                            continue;
                        }
                    };

                // Apply TLS config for https endpoints (БЕЗ ОТКЛЮЧЕНИЯ!)
                let client_builder = if config.endpoint.starts_with("https://") {
                    // Хардкодим домен Helius и используем системные сертификаты
                    let domain = config
                        .endpoint
                        .strip_prefix("https://")
                        .unwrap_or(&config.endpoint)
                        .split(':')
                        .next()
                        .unwrap_or(&config.endpoint);

                    match client_builder.tls_config(
                        ClientTlsConfig::new()
                            .with_native_roots()
                            .domain_name(domain),
                    ) {
                        Ok(builder) => builder,
                        Err(e) => {
                            error!(error = %e, "CRITICAL: Failed to set TLS config with native roots.");
                            if reconnect_attempts >= max_reconnect_attempts {
                                error!("Max reconnect attempts reached, giving up");
                                break;
                            }
                            let backoff =
                                (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                            warn!("Reconnecting in {:?}...", backoff);
                            sleep(backoff).await;
                            reconnect_attempts += 1;
                            continue;
                        }
                    }
                } else {
                    client_builder
                };

                // Apply x-token if provided
                let client_builder = if let Some(token) = config.x_token.clone() {
                    match client_builder.x_token(Some(token)) {
                        Ok(builder) => builder,
                        Err(e) => {
                            error!(error = %e, "Failed to set x-token");
                            if reconnect_attempts >= max_reconnect_attempts {
                                error!("Max reconnect attempts reached, giving up");
                                break;
                            }
                            let backoff =
                                (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                            warn!("Reconnecting in {:?}...", backoff);
                            sleep(backoff).await;
                            reconnect_attempts += 1;
                            continue;
                        }
                    }
                } else {
                    client_builder
                };

                let mut client = match client_builder.connect().await {
                    Ok(c) => c,
                    Err(e) => {
                        error!(error = %e, "Failed to connect to Geyser gRPC");
                        let token_preview = config
                            .x_token
                            .as_ref()
                            .and_then(|t| t.get(..8))
                            .unwrap_or("none");
                        error!("Endpoint: {}, Token: {}", config.endpoint, token_preview);
                        if reconnect_attempts >= max_reconnect_attempts {
                            error!("Max reconnect attempts reached, giving up");
                            break;
                        }
                        let backoff =
                            (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                        warn!("Reconnecting in {:?}...", backoff);
                        sleep(backoff).await;
                        reconnect_attempts += 1;
                        continue;
                    }
                };

                info!("Connected to Geyser gRPC");

                // Perform backfill if this is a reconnection and backfill provider is configured
                let was_reconnection = reconnect_attempts > 0;
                reconnect_attempts = 0; // Reset on successful connection

                if was_reconnection {
                    if let Some(ref backfill) = backfill_provider {
                        info!("Performing backfill for open positions after reconnection");
                        match backfill.perform_backfill().await {
                            Ok(count) => {
                                info!(positions_checked = count, "Backfill completed");
                            }
                            Err(e) => {
                                warn!(error = %e, "Backfill failed, continuing with subscription");
                            }
                        }
                    } else {
                        info!("No backfill provider configured, skipping position check");
                    }
                }

                // Subscribe to account updates for specific program owners (DEX programs)
                // This filters accounts by owner (program_id) to receive all account updates
                // owned by the specified DEX programs (Raydium, Meteora, Pump.fun, etc.)
                let mut accounts_map = HashMap::new();
                let owner_strings: Vec<String> =
                    program_ids.iter().map(|p| p.to_string()).collect();

                accounts_map.insert(
                    "dex_accounts".to_string(),
                    SubscribeRequestFilterAccounts {
                        owner: owner_strings,
                        ..Default::default()
                    },
                );

                let accounts_count = accounts_map.len();

                let subscribe_request = SubscribeRequest {
                    accounts: accounts_map,
                    transactions: HashMap::new(), // Empty - we use account-based subscription
                    commitment: Some(config.commitment),
                    ..Default::default()
                };

                info!(
                    accounts_count,
                    has_token = config.x_token.is_some(),
                    "Creating Geyser account-based subscription"
                );

                let mut stream = match client.subscribe_once(subscribe_request).await {
                    Ok(s) => s,
                    Err(e) => {
                        error!(error = %e, "Failed to subscribe to Geyser gRPC");
                        if reconnect_attempts >= max_reconnect_attempts {
                            error!("Max reconnect attempts reached, giving up");
                            break;
                        }
                        let backoff =
                            (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                        warn!("Reconnecting in {:?}...", backoff);
                        sleep(backoff).await;
                        reconnect_attempts += 1;
                        continue;
                    }
                };

                info!("Subscribed to Geyser gRPC");

                use tokio_stream::StreamExt;
                let mut total_updates = 0u64;
                let mut account_updates_count = 0u64;
                loop {
                    match stream.next().await {
                        Some(Ok(update)) => {
                            total_updates += 1;
                            if total_updates.is_multiple_of(100) {
                                info!(
                                    total_updates,
                                    account_updates_count, "Geyser stream progress"
                                );
                            }

                            if let Some(update_oneof) = update.update_oneof {
                                match update_oneof {
                                    yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof::Account(account_update) => {
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

                                            account_updates_count += 1;

                                            let account_update = AccountUpdate {
                                                account: pubkey,
                                                slot: account_update.slot,
                                                lamports: account.lamports,
                                                data: account.data,
                                                owner,
                                                executable: account.executable,
                                                rent_epoch: account.rent_epoch,
                                                timestamp_ms: std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_millis() as u64,
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
                                    yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof::Transaction(tx_update) => {
                                        // Transaction updates are not used in account-based subscription
                                        // Log for debugging purposes only
                                        tracing::debug!(
                                            slot = tx_update.slot,
                                            "Received transaction update (not processed in account-based mode)"
                                        );
                                    }
                                    _ => {} // Ignore other update types
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
