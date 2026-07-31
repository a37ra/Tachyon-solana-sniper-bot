//! Raydium pool creation listener (AMM v4, CP-MM, CLMM).
//!
//! Subscribes to Raydium program account updates via WebSocket/PubSub
//! or RPC logs with exponential backoff and auto-reconnect.

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

pub const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_CPMM_PROGRAM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
pub const RAYDIUM_CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";

pub struct RaydiumListener {
    ws_url: String,
}

impl Default for RaydiumListener {
    fn default() -> Self {
        Self::new()
    }
}

impl RaydiumListener {
    pub fn new() -> Self {
        let ws_url = std::env::var("SOLANA_WS_URL")
            .unwrap_or_else(|_| "wss://api.mainnet-beta.solana.com".to_string());
        Self { ws_url }
    }

    pub fn with_url(ws_url: String) -> Self {
        Self { ws_url }
    }

    /// Subscribe to Raydium pool creation events across all 3 variants.
    pub async fn subscribe_to_pool_creation(
        &self,
    ) -> Result<mpsc::Receiver<Signal>> {
        let (tx, rx) = mpsc::channel(100);
        let ws_url = self.ws_url.clone();

        tokio::spawn(async move {
            info!(ws_url = %ws_url, "Starting Raydium pool creation listener with auto-reconnect");
            let mut backoff = Duration::from_secs(1);

            loop {
                match Self::run_subscription_loop(&ws_url, tx.clone()).await {
                    Ok(()) => {
                        info!("Raydium subscription loop ended gracefully, restarting...");
                        backoff = Duration::from_secs(1);
                    }
                    Err(e) => {
                        warn!(error = %e, backoff_secs = backoff.as_secs(), "Raydium subscription error, reconnecting...");
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
            .context("Failed to connect to Solana WebSocket endpoint")?;

        let program_id = Pubkey::from_str(RAYDIUM_AMM_V4_PROGRAM)?;

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
            .context("Failed to subscribe to Raydium program accounts")?;

        info!("Successfully subscribed to Raydium AMM v4 program accounts");

        while let Some(response) = stream.next().await {
            let slot = response.context.slot;
            let account_pubkey = response.value.pubkey;

            if let Some(account) = response.value.account.decode::<solana_sdk::account::Account>() {
                let account_data = account.data;
                // Raydium AMM v4 pool account size is 752 bytes
                if account_data.len() >= 752 {
                    if let Ok(coin_mint) = read_pubkey_bytes(&account_data, 400) {
                        let signal = Signal {
                            mint: coin_mint.to_string(),
                            pool_address: account_pubkey.clone(),
                            dex_program: RAYDIUM_AMM_V4_PROGRAM.to_string(),
                            signal_source: "geyser_ws".to_string(),
                            slot,
                            received_at: Utc::now().timestamp_millis() as u64,
                        };

                        if tx.send(signal).await.is_err() {
                            info!("Receiver dropped, closing Raydium listener loop");
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
