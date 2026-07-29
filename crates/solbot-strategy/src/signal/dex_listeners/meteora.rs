//! Meteora pool creation listener (DLMM, Dynamic AMM).
//!
//! Subscribes to Meteora DLMM / Dynamic AMM program accounts via WebSocket
//! with automatic reconnect and exponential backoff.

use solbot_common::types::Signal;
use anyhow::{Context, Result};
use chrono::Utc;
use futures_util::StreamExt;
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_account_decoder::UiAccountEncoding;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub const METEORA_DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp";
pub const METEORA_DYNAMIC_PROGRAM: &str = "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB";

pub struct MeteoraListener {
    ws_url: String,
}

impl MeteoraListener {
    pub fn new() -> Self {
        let ws_url = std::env::var("SOLANA_WS_URL")
            .unwrap_or_else(|_| "wss://api.mainnet-beta.solana.com".to_string());
        Self { ws_url }
    }

    pub fn with_url(ws_url: String) -> Self {
        Self { ws_url }
    }

    pub async fn subscribe_to_pool_creation(
        &self,
    ) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(100);
        let ws_url = self.ws_url.clone();

        tokio::spawn(async move {
            info!(ws_url = %ws_url, "Starting Meteora pool creation listener");
            let mut backoff = Duration::from_secs(1);

            loop {
                match Self::run_subscription_loop(&ws_url, tx.clone()).await {
                    Ok(()) => {
                        info!("Meteora subscription loop ended gracefully, restarting...");
                        backoff = Duration::from_secs(1);
                    }
                    Err(e) => {
                        warn!(error = %e, backoff_secs = backoff.as_secs(), "Meteora subscription error, reconnecting...");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(60));
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn run_subscription_loop(
        ws_url: &str,
        tx: mpsc::Sender<Signal>,
    ) -> Result<()> {
        let pubsub = PubsubClient::new(ws_url)
            .await
            .context("Failed to connect to WebSocket endpoint")?;

        let program_id = Pubkey::from_str(METEORA_DLMM_PROGRAM)?;

        let config = RpcProgramAccountsConfig {
            filters: None,
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                data_slice: None,
                commitment: Some(solana_sdk::commitment_config::CommitmentConfig::confirmed()),
                min_context_slot: None,
            },
            with_context: Some(true),
        };

        let (mut stream, _unsub) = pubsub
            .program_subscribe(&program_id, Some(config))
            .await
            .context("Failed to subscribe to Meteora DLMM program accounts")?;

        info!("Successfully subscribed to Meteora DLMM program accounts");

        while let Some(response) = stream.next().await {
            let slot = response.context.slot;
            let account_pubkey = response.value.pubkey;

            if let Some(account) = response.value.account.decode::<solana_sdk::account::Account>() {
                let account_data = account.data;
                // LB Pair minimum length
                if account_data.len() >= 8 + 32 * 6 {
                    let offset = 8 + 32 * 2; // token_x_mint offset
                    if let Ok(mint_x) = read_pubkey_bytes(&account_data, offset) {
                        let signal = Signal {
                            mint: mint_x.to_string(),
                            pool_address: account_pubkey.clone(),
                            dex_program: METEORA_DLMM_PROGRAM.to_string(),
                            signal_source: "geyser_ws".to_string(),
                            slot,
                            received_at: Utc::now().timestamp_millis() as u64,
                        };

                        if tx.send(signal).await.is_err() {
                            info!("Receiver dropped, closing Meteora listener loop");
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

fn read_pubkey_bytes(data: &[u8], offset: usize) -> Result<Pubkey> {
    if offset + 32 > data.len() {
        anyhow::bail!("Pubkey offset out of bounds");
    }
    let bytes: [u8; 32] = data[offset..offset + 32].try_into()?;
    Ok(Pubkey::new_from_array(bytes))
}
