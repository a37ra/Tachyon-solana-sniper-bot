//! Public RPC Fallback (Routing 4.2).
//!
//! Implements fallback transaction submission via public RPC endpoints
//! when Jito bundles are unavailable or fail.
//!
//! Features:
//! - Multiple RPC endpoint support with health checking
//! - Retry logic with exponential backoff
//! - Priority fee estimation and inclusion
//! - Transaction confirmation polling

use super::SubmitResult;
use anyhow::{anyhow, Result};
use solana_rpc_client::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_transaction::Transaction;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// RPC endpoint configuration with health status.
#[derive(Debug, Clone)]
pub struct RpcEndpoint {
    pub url: String,
    pub name: String,
    pub healthy: bool,
    pub last_check: Option<Instant>,
    pub latency_ms: f64,
}

/// Public RPC client configuration.
#[derive(Debug, Clone)]
pub struct PublicRpcConfig {
    pub endpoints: Vec<RpcEndpoint>,
    pub max_retries: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub confirmation_timeout_ms: u64,
    pub confirmation_poll_interval_ms: u64,
}

impl Default for PublicRpcConfig {
    fn default() -> Self {
        Self {
            endpoints: vec![
                RpcEndpoint {
                    url: "https://api.mainnet-beta.solana.com".to_string(),
                    name: "solana-mainnet".to_string(),
                    healthy: true,
                    last_check: None,
                    latency_ms: 0.0,
                },
                RpcEndpoint {
                    url: "https://solana-api.projectserum.com".to_string(),
                    name: "project-serum".to_string(),
                    healthy: true,
                    last_check: None,
                    latency_ms: 0.0,
                },
                RpcEndpoint {
                    url: "https://rpc.ankr.com/solana".to_string(),
                    name: "ankr".to_string(),
                    healthy: true,
                    last_check: None,
                    latency_ms: 0.0,
                },
            ],
            max_retries: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 2000,
            confirmation_timeout_ms: 10000,
            confirmation_poll_interval_ms: 500,
        }
    }
}

/// Public RPC client with fallback and retry logic.
pub struct PublicRpcClient {
    config: PublicRpcConfig,
    current_endpoint_index: usize,
}

impl PublicRpcClient {
    pub fn new(config: PublicRpcConfig) -> Self {
        Self {
            config,
            current_endpoint_index: 0,
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(PublicRpcConfig::default())
    }

    /// Get the current RPC endpoint.
    fn current_endpoint(&self) -> &RpcEndpoint {
        &self.config.endpoints[self.current_endpoint_index]
    }

    /// Rotate to the next healthy endpoint.
    fn rotate_endpoint(&mut self) {
        let start = self.current_endpoint_index;
        loop {
            self.current_endpoint_index =
                (self.current_endpoint_index + 1) % self.config.endpoints.len();
            if self.config.endpoints[self.current_endpoint_index].healthy {
                break;
            }
            if self.current_endpoint_index == start {
                // All endpoints unhealthy, stay on current
                break;
            }
        }
    }

    /// Calculate exponential backoff delay.
    fn calculate_backoff(&self, attempt: u32) -> Duration {
        let backoff_ms = (self.config.initial_backoff_ms as f64 * 2_f64.powi(attempt as i32))
            .min(self.config.max_backoff_ms as f64) as u64;
        // Add jitter (±25%)
        let jitter = (backoff_ms as f64 * 0.25 * (rand::random::<f64>() - 0.5) * 2.0) as i64;
        let final_ms = (backoff_ms as i64 + jitter).max(0) as u64;
        Duration::from_millis(final_ms)
    }

    /// Submit transaction with retry logic and fallback.
    /// Submit a fully assembled and signed transaction via Public RPC.
    ///
    /// The transaction MUST already contain the desired compute budget /
    /// priority fee instructions before signing. This pathway does not mutate
    /// transaction instructions because the signature is already fixed.
    pub async fn submit_transaction(
        &mut self,
        transaction: Transaction,
    ) -> Result<SubmitResult> {
        let endpoint_name = self.current_endpoint().name.clone();
        let endpoint_url = self.current_endpoint().url.clone();
        let rpc = RpcClient::new(endpoint_url.clone());

        tracing::info!(
            endpoint = %endpoint_name,
            url = %endpoint_url,
            "Submitting signed transaction via Public RPC"
        );

        let mut last_error = None;

        for attempt in 0..=self.config.max_retries {
            let result = self.submit_with_retry(&rpc, &transaction, attempt).await;

            match result {
                Ok(result) => return Ok(result),
                Err(e) => {
                    tracing::warn!(
                        endpoint = %endpoint_name,
                        attempt = attempt,
                        error = %e,
                        "Transaction submission failed, retrying"
                    );
                    last_error = Some(e);

                    // Rotate endpoint on failure
                    self.rotate_endpoint();

                    // Exponential backoff before retry
                    if attempt < self.config.max_retries {
                        let backoff = self.calculate_backoff(attempt);
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }

        Err(anyhow!(
            "All retries exhausted. Last error: {}",
            last_error.unwrap_or_else(|| anyhow!("Unknown error"))
        ))
    }

    /// Submit transaction to a specific RPC endpoint.
    async fn submit_with_retry(
        &self,
        rpc: &RpcClient,
        transaction: &Transaction,
        _attempt: u32,
    ) -> Result<SubmitResult> {
        let rpc_url = rpc.url();
        let tx_clone = transaction.clone();

        tracing::debug!(
            endpoint = %rpc_url,
            "Public RPC submission received signed transaction; compute budget must already be baked in",
        );

        // Submit transaction
        let signature = tokio::task::spawn_blocking(move || {
            let rpc = RpcClient::new(rpc_url);
            rpc.send_transaction(&tx_clone)
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))??;

        let signature_str = signature.to_string();
        tracing::debug!(signature = %signature_str, "Transaction submitted successfully");

        // Poll for confirmation
        self.poll_confirmation(rpc, &signature_str).await
    }

    /// Poll for transaction confirmation.
    async fn poll_confirmation(&self, rpc: &RpcClient, signature: &str) -> Result<SubmitResult> {
        let start = Instant::now();
        let timeout = Duration::from_millis(self.config.confirmation_timeout_ms);
        let interval = Duration::from_millis(self.config.confirmation_poll_interval_ms);

        let sig = Signature::from_str(signature)
            .map_err(|e| anyhow!("Invalid signature: {}", e))?;
        let rpc_url = rpc.url().to_string();

        loop {
            if start.elapsed() > timeout {
                return Ok(SubmitResult::Timeout);
            }

            let sig_clone = sig.clone();
            let rpc_url_for_poll = rpc_url.clone();
            let result = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_for_poll);
                rpc.get_signature_statuses(&[sig_clone])
            })
            .await
            .map_err(|e| anyhow!("Task join error: {}", e))??;

            if let Some(status_opt) = result.value.get(0) {
                if let Some(status) = status_opt.clone() {
                    if status.err.is_some() {
                        tracing::warn!(error = ?status.err, "Transaction failed");
                        return Ok(SubmitResult::Failed {
                            reason: format!("Transaction failed: {:?}", status.err),
                        });
                    }

                    return Ok(SubmitResult::Confirmed {
                        signature: signature.to_string(),
                        slot: status.slot,
                    });
                }
            }

            tokio::time::sleep(interval).await;
        }
    }

    /// Health check all RPC endpoints.
    pub async fn health_check_all(&mut self) -> Vec<RpcEndpoint> {
        let mut tasks = Vec::new();

        for endpoint in self.config.endpoints.clone() {
            let endpoint_clone = endpoint.clone();
            tasks.push((
                endpoint_clone.clone(),
                tokio::spawn(async move {
                    Self::health_check_endpoint(&endpoint_clone).await
                }),
            ));
        }

        let mut results = Vec::new();
        for (endpoint, task) in tasks {
            if let Ok(result) = task.await {
                results.push(result);
            } else {
                results.push(RpcEndpoint {
                    healthy: false,
                    latency_ms: 0.0,
                    last_check: Some(Instant::now()),
                    ..endpoint
                });
            }
        }

        // Update endpoints with health status
        self.config.endpoints = results.clone();

        results
    }

    /// Health check a single RPC endpoint.
    async fn health_check_endpoint(endpoint: &RpcEndpoint) -> RpcEndpoint {
        let rpc = RpcClient::new(endpoint.url.clone());
        let start = Instant::now();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(move || rpc.get_health()),
        )
        .await;

        let latency_ms = start.elapsed().as_secs_f64() * 1000.0;

        let healthy = match result {
            Ok(Ok(Ok(_))) => true,
            _ => false,
        };

        tracing::debug!(
            endpoint = %endpoint.name,
            healthy = healthy,
            latency_ms = latency_ms,
            "Health check result"
        );

        RpcEndpoint {
            healthy,
            latency_ms,
            last_check: Some(Instant::now()),
            ..endpoint.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_public_rpc_config_default() {
        let config = PublicRpcConfig::default();
        assert_eq!(config.endpoints.len(), 3);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_backoff_ms, 100);
        assert_eq!(config.max_backoff_ms, 2000);
    }

    #[test]
    fn test_public_rpc_client_creation() {
        let client = PublicRpcClient::with_default_config();
        assert_eq!(client.config.endpoints.len(), 3);
        assert_eq!(client.current_endpoint_index, 0);
    }

    #[test]
    fn test_calculate_backoff() {
        let config = PublicRpcConfig::default();
        let client = PublicRpcClient::new(config);

        let backoff_0 = client.calculate_backoff(0);
        let backoff_1 = client.calculate_backoff(1);
        let backoff_2 = client.calculate_backoff(2);

        // Exponential growth
        assert!(backoff_1 > backoff_0);
        assert!(backoff_2 > backoff_1);

        // Should not exceed max
        assert!(backoff_2 <= Duration::from_millis(2000));
    }

    #[test]
    fn test_endpoint_rotation() {
        let config = PublicRpcConfig::default();
        let mut client = PublicRpcClient::new(config);

        let initial_index = client.current_endpoint_index;
        client.rotate_endpoint();
        assert_ne!(client.current_endpoint_index, initial_index);
    }
}
