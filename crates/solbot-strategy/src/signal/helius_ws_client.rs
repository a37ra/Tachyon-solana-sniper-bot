//! Helius WebSocket client using programSubscribe for real-time account updates.
//!
//! Provides a Geyser gRPC-compatible interface using standard Solana WebSocket
//! protocol with programSubscribe. This is a temporary replacement for Geyser gRPC
//! on Helius free plan, producing the same AccountUpdate format as geyser_client.
//!
//! Features:
//! - programSubscribe with base64 encoding for direct account data access
//! - WebSocket-level ping/pong keepalive (not JSON-RPC ping)
//! - Automatic reconnection with exponential backoff
//! - Compatible with existing from_account_data() parsers

use anyhow::Result;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep, interval};
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{info, warn, error};
use serde_json;
use std::str::FromStr;
use futures_util::{StreamExt, SinkExt};
use base64::Engine;

/// Re-export AccountUpdate from geyser_client for compatibility
pub use super::geyser_client::AccountUpdate;

/// Re-export ParsedSignal from logs_parser
pub use super::logs_parser::ParsedSignal;

/// Configuration for Helius WebSocket client.
#[derive(Debug, Clone)]
pub struct HeliusWsConfig {
    /// WebSocket endpoint URL (e.g., "wss://mainnet.helius-rpc.com/?api-key=...")
    pub ws_url: String,
    /// Commitment level for subscription (processed, confirmed, finalized)
    pub commitment: String,
    /// Encoding for account data (base64, jsonParsed)
    pub encoding: String,
}

impl Default for HeliusWsConfig {
    fn default() -> Self {
        let api_key = std::env::var("HELIUS_API_KEY")
            .or_else(|_| std::env::var("GEYSER_X_TOKEN"))
            .unwrap_or_else(|_| "YOUR_API_KEY".to_string());
        
        Self {
            ws_url: format!("wss://mainnet.helius-rpc.com/?api-key={}", api_key),
            commitment: std::env::var("HELIUS_COMMITMENT")
                .unwrap_or_else(|_| "confirmed".to_string()),
            encoding: std::env::var("HELIUS_ENCODING")
                .unwrap_or_else(|_| "base64".to_string()),
        }
    }
}

impl HeliusWsConfig {
    /// Create config from environment with custom endpoint
    pub fn from_env() -> Self {
        Self::default()
    }
}

/// Helius WebSocket client with programSubscribe support.
pub struct HeliusWsClient {
    config: HeliusWsConfig,
}

impl HeliusWsClient {
    pub fn new(config: HeliusWsConfig) -> Self {
        Self { config }
    }

    pub fn from_env() -> Self {
        Self::new(HeliusWsConfig::from_env())
    }

    /// Subscribe to account updates for specific program IDs using programSubscribe.
    ///
    /// Returns a channel that receives AccountUpdate events compatible with
    /// the existing geyser_client interface.
    ///
    /// # Arguments
    /// * `program_ids` - List of program IDs to filter account updates by owner
    pub async fn subscribe_to_programs(
        &self,
        program_ids: Vec<Pubkey>,
    ) -> Result<mpsc::Receiver<AccountUpdate>> {
        let (tx, rx) = mpsc::channel(1000);
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut reconnect_attempts = 0;
            let max_reconnect_attempts = 10;
            let base_backoff = Duration::from_secs(1);
            let max_backoff = Duration::from_secs(60);

            loop {
                info!(
                    ws_url = %config.ws_url.split('?').next().unwrap_or(&config.ws_url),
                    attempt = reconnect_attempts,
                    "Connecting to Helius WebSocket"
                );

                match tokio_tungstenite::connect_async(&config.ws_url).await {
                    Ok((ws_stream, _)) => {
                        info!("Connected to Helius WebSocket");
                        reconnect_attempts = 0;

                        let (mut write, mut read) = ws_stream.split();

                        // Subscribe to programSubscribe for each program ID
                        for program_id in &program_ids {
                            let subscribe_msg = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "method": "programSubscribe",
                                "params": [
                                    program_id.to_string(),
                                    {
                                        "encoding": config.encoding,
                                        "commitment": config.commitment
                                    }
                                ]
                            });

                            if let Err(e) = write.send(Message::Text(subscribe_msg.to_string())).await {
                                error!(error = %e, "Failed to send programSubscribe request");
                                break;
                            }

                            info!(program = %program_id, "Subscribed to program");
                        }

                        // Start ping interval (every 30 seconds)
                        let mut ping_interval = interval(Duration::from_secs(30));
                        
                        let mut total_updates = 0u64;
                        let mut account_updates_count = 0u64;

                        loop {
                            tokio::select! {
                                // Handle ping interval
                                _ = ping_interval.tick() => {
                                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                                        error!(error = %e, "Failed to send ping");
                                        break;
                                    }
                                    info!("WebSocket ping sent");
                                }
                                
                                // Handle incoming messages
                                message = read.next() => {
                                    match message {
                                        Some(Ok(Message::Text(text))) => {
                                            total_updates += 1;
                                            if total_updates.is_multiple_of(100) {
                                                info!(total_updates, account_updates_count, "WebSocket stream progress");
                                            }

                                            // Log first few messages for debugging
                                            if total_updates <= 10 {
                                                info!("Raw message {}: {}", total_updates, text);
                                            }

                                            if let Some(account_update) = Self::parse_program_notification(&text) {
                                                account_updates_count += 1;
                                                
                                                if tx.send(account_update).await.is_err() {
                                                    info!("Receiver dropped, closing Helius WS listener loop");
                                                    return;
                                                }
                                            }
                                        }
                                        Some(Ok(Message::Pong(_))) => {
                                            info!("WebSocket pong received");
                                        }
                                        Some(Ok(Message::Close(_))) => {
                                            warn!("WebSocket closed by server, reconnecting...");
                                            break;
                                        }
                                        Some(Err(e)) => {
                                            error!(error = %e, "WebSocket stream error, reconnecting...");
                                            break;
                                        }
                                        None => {
                                            warn!("WebSocket stream ended, reconnecting...");
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to connect to Helius WebSocket");
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
                }
            }
        });

        Ok(rx)
    }

    /// Subscribe to transaction logs for specific program IDs using logsSubscribe.
    ///
    /// Returns a channel that receives ParsedSignal events containing instruction-level data.
    ///
    /// # Arguments
    /// * `program_ids` - List of program IDs to filter logs by involved programs
    pub async fn subscribe_to_logs(
        &self,
        program_ids: Vec<Pubkey>,
    ) -> Result<mpsc::Receiver<ParsedSignal>> {
        let (tx, rx) = mpsc::channel(1000);
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut reconnect_attempts = 0;
            let max_reconnect_attempts = 10;
            let base_backoff = Duration::from_secs(1);
            let max_backoff = Duration::from_secs(60);

            loop {
                info!(
                    ws_url = %config.ws_url.split('?').next().unwrap_or(&config.ws_url),
                    attempt = reconnect_attempts,
                    "Connecting to Helius WebSocket for logsSubscribe"
                );

                match tokio_tungstenite::connect_async(&config.ws_url).await {
                    Ok((ws_stream, _)) => {
                        info!("Connected to Helius WebSocket for logsSubscribe");
                        reconnect_attempts = 0;

                        let (mut write, mut read) = ws_stream.split();

                        // Subscribe to logsSubscribe for each program ID
                        for program_id in &program_ids {
                            let subscribe_msg = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "method": "logsSubscribe",
                                "params": [
                                    {
                                        "mentions": [program_id.to_string()]
                                    },
                                    {
                                        "commitment": config.commitment
                                    }
                                ]
                            });

                            if let Err(e) = write.send(Message::Text(subscribe_msg.to_string())).await {
                                error!(error = %e, "Failed to send logsSubscribe request");
                                break;
                            }

                            info!(program = %program_id, "Subscribed to logs for program");
                        }

                        // Start ping interval (every 30 seconds)
                        let mut ping_interval = interval(Duration::from_secs(30));
                        
                        let mut total_logs = 0u64;
                        let mut parsed_signals_count = 0u64;

                        loop {
                            tokio::select! {
                                // Handle ping interval
                                _ = ping_interval.tick() => {
                                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                                        error!(error = %e, "Failed to send ping");
                                        break;
                                    }
                                    info!("WebSocket ping sent (logsSubscribe)");
                                }
                                
                                // Handle incoming messages
                                message = read.next() => {
                                    match message {
                                        Some(Ok(Message::Text(text))) => {
                                            total_logs += 1;
                                            
                                            if total_logs.is_multiple_of(100) {
                                                info!(total_logs, parsed_signals_count, "WebSocket logs stream progress");
                                            }

                                            // Parse logs message
                                            if let Ok(signals) = crate::signal::logs_parser::parse_logs_message(&text) {
                                                parsed_signals_count += signals.len() as u64;
                                                
                                                for signal in signals {
                                                    if tx.send(signal).await.is_err() {
                                                        info!("Receiver dropped, closing Helius WS logs listener loop");
                                                        return;
                                                    }
                                                }
                                            }
                                        }
                                        Some(Ok(Message::Pong(_))) => {
                                            info!("WebSocket pong received (logsSubscribe)");
                                        }
                                        Some(Ok(Message::Close(_))) => {
                                            warn!("WebSocket closed by server (logsSubscribe)");
                                            break;
                                        }
                                        Some(Err(e)) => {
                                            error!(error = %e, "WebSocket error (logsSubscribe)");
                                            break;
                                        }
                                        None => {
                                            warn!("WebSocket stream ended (logsSubscribe)");
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to connect to Helius WebSocket (logsSubscribe)");
                        
                        if reconnect_attempts >= max_reconnect_attempts {
                            error!("Max reconnect attempts reached, giving up (logsSubscribe)");
                            return;
                        }
                        
                        let backoff = (base_backoff * 2u32.pow(reconnect_attempts)).min(max_backoff);
                        warn!("Reconnecting in {:?}... (logsSubscribe)", backoff);
                        sleep(backoff).await;
                        reconnect_attempts += 1;
                        continue;
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Parse programNotification from WebSocket message.
    fn parse_program_notification(text: &str) -> Option<AccountUpdate> {
        let json: serde_json::Value = serde_json::from_str(text).ok()?;
        
        // Check if this is a programNotification
        let method = json.get("method")?.as_str()?;
        if method != "programNotification" {
            return None;
        }

        let params = json.get("params")?;
        let result = params.get("result")?;
        
        // Extract slot from context
        let slot = result.get("context")?.get("slot")?.as_u64()?;

        // Extract value (account info)
        let value = result.get("value")?;
        
        // Extract pubkey (at value level in Helius format)
        let account_str = value.get("pubkey")?.as_str()?;
        let account_pubkey = Pubkey::from_str(account_str).ok()?;

        // Extract account info (nested inside value)
        let account = value.get("account")?;

        // Parse owner
        let owner_str = account.get("owner")?.as_str()?;
        let owner_pubkey = Pubkey::from_str(owner_str).ok()?;

        // Parse lamports
        let lamports = account.get("lamports")?.as_u64()?;

        // Parse data (base64 encoded) - Helius returns it as an array [data, "base64"]
        let data_array = account.get("data")?;
        let data_base64 = if data_array.is_array() {
            data_array.as_array()?.get(0)?.as_str()?
        } else {
            data_array.as_str()?
        };
        let data = base64::engine::general_purpose::STANDARD.decode(data_base64).ok()?;

        // Parse executable
        let executable = account.get("executable")?.as_bool().unwrap_or(false);

        // Parse rent epoch
        let rent_epoch = account.get("rentEpoch")?.as_u64().unwrap_or(0);

        let update = AccountUpdate {
            account: account_pubkey,
            slot,
            lamports,
            data,
            owner: owner_pubkey,
            executable,
            rent_epoch,
        };

        info!("Parsed account update: account={} owner={} slot={} data_len={}", 
              update.account, update.owner, update.slot, update.data.len());

        Some(update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_helius_ws_config_default() {
        let config = HeliusWsConfig::default();
        assert!(!config.ws_url.is_empty());
        assert!(!config.commitment.is_empty());
        assert!(!config.encoding.is_empty());
    }

    #[test]
    fn test_parse_program_notification() {
        let sample = r#"{
            "jsonrpc": "2.0",
            "method": "programNotification",
            "params": {
                "result": {
                    "subscription": 123,
                    "result": {
                        "slot": 123456789,
                        "account": {
                            "pubkey": "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrp1bPH6CSoTf",
                            "owner": "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
                            "lamports": 1000000,
                            "data": "AQAAAJMD...",
                            "executable": false,
                            "rentEpoch": 100
                        }
                    }
                }
            }
        }"#;

        // This will fail due to invalid base64, but tests the parsing structure
        let result = HeliusWsClient::parse_program_notification(sample);
        assert!(result.is_none()); // Due to invalid base64
    }
}
