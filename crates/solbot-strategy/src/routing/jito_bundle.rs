//! Jito Bundle Submission (Routing 4.1).
//!
//! Implements HTTP/gRPC client to Jito block engine for MEV-protected
//! transaction submission via bundles.
//!
//! Features:
//! - Bundle formation (signed tx + tip instruction)
//! - Randomized micro-jitter for tip amounts
//! - Bundle status polling and confirmation
//! - Auto-region selection by RTT

use super::SubmitResult;
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::Request;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_sdk::system_instruction;
use solana_transaction::Transaction;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

/// Jito region configuration with endpoint URLs.
#[derive(Debug, Clone)]
pub struct JitoRegion {
    pub name: String,
    pub http_endpoint: String,
    pub grpc_endpoint: String,
}

/// Result of RTT ping to a Jito region.
#[derive(Debug, Clone)]
pub struct JitoRegionPingResult {
    pub name: String,
    pub endpoint: String,
    pub grpc_endpoint: String,
    pub rtt_ms: f64,
    pub success: bool,
    pub error: Option<String>,
}

/// Jito tip configuration.
#[derive(Debug, Clone)]
pub struct TipConfig {
    /// Base tip amount in lamports.
    pub base_lamports: u64,
    /// Random jitter percentage (e.g., 0.1 for ±10%).
    pub jitter_percent: f64,
    /// Jito tip account to receive tips.
    pub tip_account: Pubkey,
}

impl Default for TipConfig {
    fn default() -> Self {
        Self {
            base_lamports: 10_000, // 0.00001 SOL
            jitter_percent: 0.1,   // ±10%
            // Use a placeholder pubkey - in production configure actual Jito tip account
            tip_account: Pubkey::new_unique(),
        }
    }
}

/// Bundle submission configuration.
#[derive(Debug, Clone)]
pub struct BundleConfig {
    pub regions: Vec<JitoRegion>,
    pub tip_config: TipConfig,
    pub poll_timeout_ms: u64,
    pub poll_interval_ms: u64,
}

impl Default for BundleConfig {
    fn default() -> Self {
        Self {
            regions: vec![
                JitoRegion {
                    name: "amsterdam".to_string(),
                    http_endpoint: "https://amsterdam.mainnet.block-engine.jito.wtf/api/v1/bundles".to_string(),
                    grpc_endpoint: "amsterdam.mainnet.block-engine.jito.wtf:443".to_string(),
                },
                JitoRegion {
                    name: "frankfurt".to_string(),
                    http_endpoint: "https://frankfurt.mainnet.block-engine.jito.wtf/api/v1/bundles".to_string(),
                    grpc_endpoint: "frankfurt.mainnet.block-engine.jito.wtf:443".to_string(),
                },
                JitoRegion {
                    name: "ny".to_string(),
                    http_endpoint: "https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles".to_string(),
                    grpc_endpoint: "ny.mainnet.block-engine.jito.wtf:443".to_string(),
                },
                JitoRegion {
                    name: "tokyo".to_string(),
                    http_endpoint: "https://tokyo.mainnet.block-engine.jito.wtf/api/v1/bundles".to_string(),
                    grpc_endpoint: "tokyo.mainnet.block-engine.jito.wtf:443".to_string(),
                },
            ],
            tip_config: TipConfig::default(),
            poll_timeout_ms: 5000,
            poll_interval_ms: 200,
        }
    }
}

/// Jito Block Engine client for bundle submission.
pub struct JitoClient {
    config: BundleConfig,
    selected_region: Option<JitoRegion>,
    http_client: hyper_util::client::legacy::Client<HttpConnector, Full<Bytes>>,
}

impl JitoClient {
    pub fn new(config: BundleConfig) -> Self {
        let http_client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
        Self {
            config,
            selected_region: None,
            http_client,
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(BundleConfig::default())
    }

    /// Select the best Jito region by RTT ping.
    pub async fn auto_select_region(&mut self) -> Result<&JitoRegion> {
        let regions: Vec<(&str, &str)> = self
            .config
            .regions
            .iter()
            .map(|r| (r.name.as_str(), r.http_endpoint.as_str()))
            .collect();

        let ping_results = Self::ping_all_regions(&regions).await;

        // Find the best (lowest RTT) successful region
        let best_region = ping_results
            .into_iter()
            .filter(|r| r.success)
            .min_by(|a, b| a.rtt_ms.partial_cmp(&b.rtt_ms).unwrap_or(std::cmp::Ordering::Equal))
            .ok_or_else(|| anyhow!("No Jito region reachable"))?;

        tracing::info!(
            region = %best_region.name,
            rtt_ms = best_region.rtt_ms,
            "Auto-selected Jito region"
        );

        let region = self
            .config
            .regions
            .iter()
            .find(|r| r.name == best_region.name)
            .ok_or_else(|| anyhow!("Region not found in config"))?;

        self.selected_region = Some(region.clone());
        Ok(region)
    }

    /// Get the currently selected region or auto-select if none.
    pub async fn get_or_select_region(&mut self) -> Result<&JitoRegion> {
        if self.selected_region.is_none() {
            self.auto_select_region().await?;
        }
        Ok(self.selected_region.as_ref().unwrap())
    }

    /// Converts a standard URL into a valid gRPC endpoint string.
    pub fn to_grpc_endpoint(url: &str) -> String {
        let stripped = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');

        if stripped.contains(':') {
            stripped.to_string()
        } else {
            format!("{}:443", stripped)
        }
    }

    /// Calculate tip amount with randomized jitter.
    fn calculate_tip(&self) -> u64 {
        let base = self.config.tip_config.base_lamports;
        let jitter = self.config.tip_config.jitter_percent;
        let random_factor = 1.0 + (rand::random::<f64>() - 0.5) * 2.0 * jitter;
        (base as f64 * random_factor) as u64
    }

    /// Create a tip instruction for Jito.
    fn create_tip_instruction(&self, payer: &Pubkey, tip_amount: u64) -> Instruction {
        system_instruction::transfer(
            payer,
            &self.config.tip_config.tip_account,
            tip_amount,
        )
    }

    /// Build a Jito bundle: swap transaction + separate tip transaction.
    ///
    /// Jito bundles can contain multiple transactions processed atomically
    /// in order. The tip is sent as a second transaction at the end of the
    /// bundle so the main swap tx's instruction layout is not modified.
    ///
    /// Both transactions share the same `recent_blockhash` from the main tx.
    pub fn build_bundle(
        &self,
        transaction: Transaction,
        payer: &Pubkey,
    ) -> Result<Vec<Transaction>> {
        let tip_amount = self.calculate_tip();
        let tip_instruction = self.create_tip_instruction(payer, tip_amount);

        // Tip is a separate standalone transaction:
        //   system::transfer(payer → jito_tip_account, tip_amount)
        // Shares the same blockhash as the main tx so they land in the same slot.
        let mut tip_tx = Transaction::new_with_payer(&[tip_instruction], Some(payer));
        tip_tx.message.recent_blockhash = transaction.message.recent_blockhash;

        tracing::debug!(
            tip_amount_lamports = tip_amount,
            tip_account = %self.config.tip_config.tip_account,
            "Built Jito bundle: [swap_tx, tip_tx]"
        );

        // Bundle order: swap first, tip last — Jito requires tip in the bundle
        Ok(vec![transaction, tip_tx])
    }

    /// Measures network TCP RTT ping to Jito Block Engine endpoints.
    pub async fn ping_region(name: &str, endpoint: &str) -> JitoRegionPingResult {
        let grpc_endpoint = Self::to_grpc_endpoint(endpoint);
        let start = Instant::now();

        // Extract host and port for TCP ping
        let host_port = grpc_endpoint.clone();
        let ping_future = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&host_port));

        match ping_future.await {
            Ok(Ok(_stream)) => {
                let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;
                JitoRegionPingResult {
                    name: name.to_string(),
                    endpoint: endpoint.to_string(),
                    grpc_endpoint,
                    rtt_ms,
                    success: true,
                    error: None,
                }
            }
            Ok(Err(e)) => JitoRegionPingResult {
                name: name.to_string(),
                endpoint: endpoint.to_string(),
                grpc_endpoint,
                rtt_ms: 9999.0,
                success: false,
                error: Some(e.to_string()),
            },
            Err(_) => JitoRegionPingResult {
                name: name.to_string(),
                endpoint: endpoint.to_string(),
                grpc_endpoint,
                rtt_ms: 9999.0,
                success: false,
                error: Some("Timeout (3s)".to_string()),
            },
        }
    }

    /// Measure RTT across all configured Jito regions and return sorted by lowest latency.
    pub async fn ping_all_regions(regions: &[(&str, &str)]) -> Vec<JitoRegionPingResult> {
        let mut tasks = Vec::new();
        for &(name, endpoint) in regions {
            let name_owned = name.to_string();
            let endpoint_owned = endpoint.to_string();
            tasks.push(tokio::spawn(async move {
                Self::ping_region(&name_owned, &endpoint_owned).await
            }));
        }

        let mut results = Vec::new();
        for task in tasks {
            if let Ok(res) = task.await {
                results.push(res);
            }
        }

        results.sort_by(|a, b| a.rtt_ms.partial_cmp(&b.rtt_ms).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    /// Submit a bundle to Jito Block Engine via HTTP.
    pub async fn submit_bundle(&mut self, bundle: Vec<Transaction>) -> Result<SubmitResult> {
        let region = self.get_or_select_region().await?;

        tracing::info!(
            region = %region.name,
            endpoint = %region.http_endpoint,
            tx_count = bundle.len(),
            "Submitting Jito bundle via HTTP"
        );

        // Serialize transactions to base64
        let txs: Vec<String> = bundle
            .iter()
            .map(|tx| {
                let serialized = bincode::serialize(&tx)?;
                Ok(BASE64.encode(&serialized))
            })
            .collect::<Result<Vec<_>>>()?;

        // Build JSON payload for Jito HTTP API
        let payload = serde_json::json!({
            "transactions": txs,
        });

        // Submit via HTTP POST
        let request = Request::builder()
            .method("POST")
            .uri(&region.http_endpoint)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(payload.to_string())))?;

        let response = self.http_client.request(request).await?;
        let status = response.status();
        let body = response.collect().await?.to_bytes();
        let response_text = String::from_utf8(body.to_vec())?;

        if status.is_success() {
            // Parse response to get bundle ID
            let response_json: serde_json::Value = serde_json::from_str(&response_text)?;
            let bundle_id = response_json
                .get("result")
                .and_then(|r| r.as_str())
                .ok_or_else(|| anyhow!("No bundle ID in response"))?;

            tracing::info!(bundle_id = %bundle_id, "Jito bundle submitted successfully");

            // Poll for bundle confirmation
            self.poll_bundle_status(bundle_id).await
        } else {
            Err(anyhow!(
                "Jito bundle submission failed: {} - {}",
                status,
                response_text
            ))
        }
    }

    /// Poll bundle status until confirmation or timeout.
    async fn poll_bundle_status(&mut self, bundle_id: &str) -> Result<SubmitResult> {
        let region = self.get_or_select_region().await?;
        let status_url = format!("{}/{}", region.http_endpoint.trim_end_matches("/bundles"), bundle_id);

        let start = Instant::now();
        let timeout = Duration::from_millis(self.config.poll_timeout_ms);
        let interval = Duration::from_millis(self.config.poll_interval_ms);

        loop {
            if start.elapsed() > timeout {
                return Ok(SubmitResult::Timeout);
            }

            let request = Request::builder()
                .method("GET")
                .uri(&status_url)
                .body(Full::new(Bytes::new()))?;

            match self.http_client.request(request).await {
                Ok(response) => {
                    let status = response.status();
                    let body = response.collect().await?.to_bytes();
                    let response_text = String::from_utf8(body.to_vec())?;

                    if status.is_success() {
                        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

                        // Check if bundle is confirmed
                        if let Some(status) = response_json.get("result").and_then(|r| r.get("status")) {
                            if status.as_str() == Some("confirmed") {
                                let signature = response_json
                                    .get("result")
                                    .and_then(|r| r.get("signature"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or(bundle_id);
                                let slot = response_json
                                    .get("result")
                                    .and_then(|r| r.get("slot"))
                                    .and_then(|s| s.as_u64())
                                    .unwrap_or(0);

                                return Ok(SubmitResult::Confirmed {
                                    signature: signature.to_string(),
                                    slot,
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Bundle status poll failed, retrying");
                }
            }

            tokio::time::sleep(interval).await;
        }
    }

    /// Ping all configured regions concurrently and return true if at least one
    /// is reachable within the 3-second TCP timeout.
    pub async fn is_any_region_healthy(&self) -> bool {
        let regions_owned: Vec<(String, String)> = self
            .config
            .regions
            .iter()
            .map(|r| (r.name.clone(), r.grpc_endpoint.clone()))
            .collect();

        let mut tasks = Vec::with_capacity(regions_owned.len());
        for (name, endpoint) in regions_owned {
            tasks.push(tokio::spawn(async move {
                Self::ping_region(&name, &endpoint).await
            }));
        }

        for task in tasks {
            if let Ok(result) = task.await {
                if result.success {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_grpc_endpoint() {
        assert_eq!(
            JitoClient::to_grpc_endpoint("https://frankfurt.mainnet.block-engine.jito.wtf"),
            "frankfurt.mainnet.block-engine.jito.wtf:443"
        );
        assert_eq!(
            JitoClient::to_grpc_endpoint("frankfurt.mainnet.block-engine.jito.wtf:443"),
            "frankfurt.mainnet.block-engine.jito.wtf:443"
        );
    }

    #[test]
    fn test_tip_config_default() {
        let config = TipConfig::default();
        assert_eq!(config.base_lamports, 10_000);
        assert_eq!(config.jitter_percent, 0.1);
    }

    #[test]
    fn test_bundle_config_default() {
        let config = BundleConfig::default();
        assert_eq!(config.regions.len(), 4);
        assert_eq!(config.poll_timeout_ms, 5000);
        assert_eq!(config.poll_interval_ms, 200);
    }

    #[test]
    fn test_jito_client_creation() {
        let client = JitoClient::with_default_config();
        assert_eq!(client.config.regions.len(), 4);
    }

    #[test]
    fn test_calculate_tip_no_jitter() {
        let mut config = BundleConfig::default();
        config.tip_config.jitter_percent = 0.0;
        config.tip_config.base_lamports = 10_000;
        let client = JitoClient::new(config);
        let tip = client.calculate_tip();
        assert_eq!(tip, 10_000);
    }

    #[test]
    fn test_calculate_tip_with_jitter() {
        let mut config = BundleConfig::default();
        config.tip_config.jitter_percent = 0.5; // ±50%
        config.tip_config.base_lamports = 10_000;
        let client = JitoClient::new(config);

        // With ±50%, tip should be between 5,000 and 15,000
        for _ in 0..10 {
            let tip = client.calculate_tip();
            assert!(tip >= 5_000 && tip <= 15_000, "Tip {} out of range", tip);
        }
    }

    /// build_bundle must return exactly two transactions:
    /// the original swap tx and a separate tip-only tx.
    #[test]
    fn test_build_bundle_produces_two_transactions() {
        use solana_sdk::hash::Hash;

        let config = BundleConfig::default();
        let client = JitoClient::new(config);
        let payer = Pubkey::new_unique();

        // Minimal unsigned transaction with a dummy blockhash
        let mut swap_tx = Transaction::new_with_payer(&[], Some(&payer));
        swap_tx.message.recent_blockhash = Hash::new_unique();

        let bundle = client.build_bundle(swap_tx.clone(), &payer).unwrap();
        assert_eq!(bundle.len(), 2, "Bundle must contain exactly 2 transactions");

        // Both transactions must share the same blockhash
        assert_eq!(
            bundle[0].message.recent_blockhash,
            bundle[1].message.recent_blockhash,
            "swap_tx and tip_tx must share the same blockhash"
        );

        // Tip tx must have exactly one instruction (SOL transfer)
        assert_eq!(
            bundle[1].message.instructions.len(), 1,
            "tip_tx must contain exactly one transfer instruction"
        );
    }
}
