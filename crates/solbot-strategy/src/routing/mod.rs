//! Transaction Routing Strategy (Section 4).
//!
//! Implements multi-stage transaction submission with automatic fallback:
//! - Stage 1: Jito Bundle Submission (priority)
//! - Stage 2: Public RPC Fallback (retries with multiple endpoints)
//! - Stage 3: Durable Nonce (emergency fallback)

pub mod durable_nonce;
pub mod jito_bundle;
pub mod public_rpc_fallback;

use anyhow::{anyhow, Result};
use durable_nonce::{DurableNonceClient, DurableNonceConfig};
use jito_bundle::{BundleConfig, JitoClient};
use public_rpc_fallback::{PublicRpcClient, PublicRpcConfig};
use solana_transaction::Transaction;

#[derive(Debug, Clone)]
pub enum SubmitResult {
    Confirmed { signature: String, slot: u64 },
    Failed { reason: String },
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingStage {
    JitoBundle,
    PublicRpc,
    DurableNonce,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RoutingStrategy {
    JitoOnly,
    PublicRpcOnly,
    DurableNonceOnly,
    AutoFallback,
}

/// Routing configuration.
#[derive(Debug, Clone)]
pub struct RoutingConfig {
    pub strategy: RoutingStrategy,
    pub jito_config: Option<BundleConfig>,
    pub public_rpc_config: Option<PublicRpcConfig>,
    pub durable_nonce_config: Option<DurableNonceConfig>,
    pub enable_jito: bool,
    pub enable_public_rpc: bool,
    pub enable_durable_nonce: bool,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            strategy: RoutingStrategy::AutoFallback,
            jito_config: Some(BundleConfig::default()),
            public_rpc_config: Some(PublicRpcConfig::default()),
            durable_nonce_config: None, // Requires nonce account setup
            enable_jito: true,
            enable_public_rpc: true,
            enable_durable_nonce: false,
        }
    }
}

/// Transaction router with multi-stage fallback.
pub struct TransactionRouter {
    config: RoutingConfig,
    jito_client: Option<JitoClient>,
    public_rpc_client: Option<PublicRpcClient>,
    durable_nonce_client: Option<DurableNonceClient>,
}

impl TransactionRouter {
    pub fn new(config: RoutingConfig) -> Result<Self> {
        let jito_client = if config.enable_jito && config.jito_config.is_some() {
            Some(JitoClient::new(config.jito_config.clone().unwrap()))
        } else {
            None
        };

        let public_rpc_client = if config.enable_public_rpc && config.public_rpc_config.is_some() {
            Some(PublicRpcClient::new(
                config.public_rpc_config.clone().unwrap(),
            ))
        } else {
            None
        };

        let durable_nonce_client =
            if config.enable_durable_nonce && config.durable_nonce_config.is_some() {
                Some(DurableNonceClient::new(
                    config.durable_nonce_config.clone().unwrap(),
                ))
            } else {
                None
            };

        Ok(Self {
            config,
            jito_client,
            public_rpc_client,
            durable_nonce_client,
        })
    }

    pub fn with_default_config() -> Result<Self> {
        Self::new(RoutingConfig::default())
    }

    /// Submit transaction using configured routing strategy.
    pub async fn submit_transaction(
        &mut self,
        transaction: Transaction,
        priority_fee_lamports: u64,
    ) -> Result<SubmitResult> {
        match self.config.strategy {
            RoutingStrategy::JitoOnly => {
                self.submit_jito_only(transaction, priority_fee_lamports)
                    .await
            }
            RoutingStrategy::PublicRpcOnly => {
                self.submit_public_rpc_only(transaction, priority_fee_lamports)
                    .await
            }
            RoutingStrategy::DurableNonceOnly => self.submit_durable_nonce_only(transaction).await,
            RoutingStrategy::AutoFallback => {
                self.submit_with_fallback(transaction, priority_fee_lamports)
                    .await
            }
        }
    }

    /// Submit with automatic fallback through stages.
    async fn submit_with_fallback(
        &mut self,
        transaction: Transaction,
        priority_fee_lamports: u64,
    ) -> Result<SubmitResult> {
        // Stage 1: Try Jito Bundle
        if let Some(client) = &mut self.jito_client {
            tracing::info!("Stage 1: Attempting Jito Bundle submission");
            // Jito bundle expects Vec<Transaction>, wrap single transaction
            let bundle = vec![transaction.clone()];
            match client.submit_bundle(bundle).await {
                Ok(result) => {
                    tracing::info!("Stage 1: Jito Bundle submission succeeded");
                    return Ok(result);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Stage 1: Jito Bundle submission failed, falling back");
                }
            }
        }

        // Stage 2: Try Public RPC
        if let Some(client) = &mut self.public_rpc_client {
            tracing::info!("Stage 2: Attempting Public RPC submission");
            match client
                .submit_transaction(transaction.clone(), priority_fee_lamports)
                .await
            {
                Ok(result) => {
                    tracing::info!("Stage 2: Public RPC submission succeeded");
                    return Ok(result);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Stage 2: Public RPC submission failed, falling back");
                }
            }
        }

        // Stage 3: Try Durable Nonce (emergency)
        if let Some(client) = &mut self.durable_nonce_client {
            tracing::warn!("Stage 3: Attempting Durable Nonce submission (emergency)");
            match client.submit_durable_nonce_tx(transaction).await {
                Ok(result) => {
                    tracing::info!("Stage 3: Durable Nonce submission succeeded");
                    return Ok(result);
                }
                Err(e) => {
                    tracing::error!(error = %e, "Stage 3: Durable Nonce submission failed");
                }
            }
        }

        Err(anyhow!("All routing stages failed"))
    }

    async fn submit_jito_only(
        &mut self,
        transaction: Transaction,
        _priority_fee_lamports: u64,
    ) -> Result<SubmitResult> {
        if let Some(client) = &mut self.jito_client {
            let bundle = vec![transaction];
            client.submit_bundle(bundle).await
        } else {
            Err(anyhow!("Jito client not configured"))
        }
    }

    async fn submit_public_rpc_only(
        &mut self,
        transaction: Transaction,
        priority_fee_lamports: u64,
    ) -> Result<SubmitResult> {
        if let Some(client) = &mut self.public_rpc_client {
            client
                .submit_transaction(transaction, priority_fee_lamports)
                .await
        } else {
            Err(anyhow!("Public RPC client not configured"))
        }
    }

    async fn submit_durable_nonce_only(
        &mut self,
        transaction: Transaction,
    ) -> Result<SubmitResult> {
        if let Some(client) = &mut self.durable_nonce_client {
            client.submit_durable_nonce_tx(transaction).await
        } else {
            Err(anyhow!("Durable Nonce client not configured"))
        }
    }

    /// Health check all enabled routing stages.
    pub async fn health_check(&mut self) -> Vec<(RoutingStage, bool)> {
        let mut results = Vec::new();

        if let Some(client) = &mut self.jito_client {
            let healthy = client.is_any_region_healthy().await;
            results.push((RoutingStage::JitoBundle, healthy));
        }

        if let Some(client) = &mut self.public_rpc_client {
            let endpoints = client.health_check_all().await;
            let healthy = endpoints.iter().any(|e| e.healthy);
            results.push((RoutingStage::PublicRpc, healthy));
        }

        if let Some(client) = &mut self.durable_nonce_client {
            let healthy = client.health_check().await;
            results.push((RoutingStage::DurableNonce, healthy));
        }

        results
    }

    /// Get current routing configuration.
    pub fn config(&self) -> &RoutingConfig {
        &self.config
    }

    /// Update routing configuration.
    pub fn update_config(&mut self, config: RoutingConfig) -> Result<()> {
        self.config = config;
        // Rebuild clients if needed
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_routing_config_default() {
        let config = RoutingConfig::default();
        assert_eq!(config.strategy, RoutingStrategy::AutoFallback);
        assert!(config.enable_jito);
        assert!(config.enable_public_rpc);
        assert!(!config.enable_durable_nonce);
    }

    #[test]
    fn test_router_creation() {
        let config = RoutingConfig::default();
        let router = TransactionRouter::new(config);
        assert!(router.is_ok());
    }

    #[test]
    fn test_router_with_default_config() {
        let router = TransactionRouter::with_default_config();
        assert!(router.is_ok());
    }
}
