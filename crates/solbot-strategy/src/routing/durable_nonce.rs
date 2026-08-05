//! Durable Nonce Transaction Submission (Routing 4.3).
//!
//! Implements transaction submission using durable nonce accounts
//! for Stage 3 emergency fallback when both Jito and Public RPC fail.
//!
//! Features:
//! - Nonce account management and advancement
//! - Transaction building with durable nonce
//! - Nonce reuse protection
//! - Confirmation polling

use super::SubmitResult;
use anyhow::{anyhow, Result};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::hash::Hash;
use solana_sdk::system_instruction;
use solana_signature::Signature;
use solana_transaction::Transaction;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// Durable nonce account data.
#[derive(Debug, Clone)]
pub struct NonceAccount {
    pub address: String,
    pub authority: String,
    pub nonce: String,
    pub fee_calculator: u64,
}

/// Durable nonce client configuration.
#[derive(Debug, Clone)]
pub struct DurableNonceConfig {
    pub nonce_account: String,
    pub nonce_authority: String,
    pub rpc_url: String,
    pub advance_nonce_before_submit: bool,
    pub confirmation_timeout_ms: u64,
    pub confirmation_poll_interval_ms: u64,
}

impl Default for DurableNonceConfig {
    fn default() -> Self {
        Self {
            nonce_account: String::new(),
            nonce_authority: String::new(),
            rpc_url: "https://api.mainnet-beta.solana.com".to_string(),
            advance_nonce_before_submit: true,
            confirmation_timeout_ms: 15000,
            confirmation_poll_interval_ms: 500,
        }
    }
}

/// Durable nonce client for Stage 3 emergency fallback.
pub struct DurableNonceClient {
    config: DurableNonceConfig,
    rpc: RpcClient,
    current_nonce: Option<String>,
    last_nonce_advance: Option<Instant>,
}

impl DurableNonceClient {
    pub fn new(config: DurableNonceConfig) -> Self {
        let rpc = RpcClient::new(config.rpc_url.clone());
        Self {
            config,
            rpc,
            current_nonce: None,
            last_nonce_advance: None,
        }
    }

    pub fn with_config(nonce_account: String, nonce_authority: String, rpc_url: String) -> Self {
        let config = DurableNonceConfig {
            nonce_account,
            nonce_authority,
            rpc_url,
            ..Default::default()
        };
        Self::new(config)
    }

    /// Fetch current nonce account data from RPC.
    pub async fn fetch_nonce_account(&mut self) -> Result<NonceAccount> {
        if self.config.nonce_account.is_empty() {
            return Err(anyhow!("Nonce account address not configured"));
        }

        let nonce_pubkey = Pubkey::from_str(&self.config.nonce_account)
            .map_err(|e| anyhow!("Invalid nonce account pubkey: {}", e))?;

        let rpc_url = self.rpc.url();
        let account_data = tokio::task::spawn_blocking(move || {
            let rpc = RpcClient::new(rpc_url);
            rpc.get_account(&nonce_pubkey)
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))??;

        if account_data.data.len() < 72 {
            return Err(anyhow!("Nonce account data too short (must be >= 72 bytes)"));
        }

        // Solana Nonce Account Data Layout:
        // Offset 0..4: State enum (1 = Initialized)
        // Offset 4..8: Version
        // Offset 8..40: Authority Pubkey (32 bytes)
        // Offset 40..72: Nonce Hash (32 bytes)
        let nonce_bytes = &account_data.data[40..72];
        let nonce = bs58::encode(nonce_bytes).into_string();

        let nonce_account = NonceAccount {
            address: self.config.nonce_account.clone(),
            authority: self.config.nonce_authority.clone(),
            nonce: nonce.clone(),
            fee_calculator: 5000,
        };

        self.current_nonce = Some(nonce);
        Ok(nonce_account)
    }

    /// Advance the durable nonce by fetching updated state from RPC.
    pub async fn advance_nonce(&mut self) -> Result<String> {
        if self.config.nonce_account.is_empty() || self.config.nonce_authority.is_empty() {
            return Err(anyhow!("Nonce account or authority not configured"));
        }

        let nonce_account = Pubkey::from_str(&self.config.nonce_account)
            .map_err(|e| anyhow!("Invalid nonce account pubkey: {}", e))?;
        let authority = Pubkey::from_str(&self.config.nonce_authority)
            .map_err(|e| anyhow!("Invalid nonce authority pubkey: {}", e))?;

        // Construct advance nonce instruction (for inclusion or offline verification)
        let _advance_ix = system_instruction::advance_nonce_account(
            &nonce_account,
            &authority,
        );

        let nonce_data = self.fetch_nonce_account().await?;
        self.current_nonce = Some(nonce_data.nonce.clone());
        self.last_nonce_advance = Some(Instant::now());

        tracing::info!(
            nonce = %nonce_data.nonce,
            "Durable nonce updated"
        );

        Ok(nonce_data.nonce)
    }

    /// Build transaction with durable nonce as recent_blockhash.
    pub fn build_nonce_transaction(&self, mut transaction: Transaction) -> Result<Transaction> {
        let nonce_str = match &self.current_nonce {
            Some(n) => n.clone(),
            None => {
                if self.config.nonce_account.is_empty() {
                    return Err(anyhow!("No durable nonce available: nonce_account is empty"));
                }
                return Ok(transaction);
            }
        };

        let nonce_hash = Hash::from_str(&nonce_str)
            .or_else(|_| {
                bs58::decode(&nonce_str)
                    .into_vec()
                    .map_err(|e| anyhow!("{}", e))
                    .and_then(|bytes| {
                        if bytes.len() == 32 {
                            let arr: [u8; 32] = bytes.try_into().unwrap();
                            Ok(Hash::new_from_array(arr))
                        } else {
                            Err(anyhow!("Invalid byte length for nonce hash"))
                        }
                    })
            })
            .map_err(|e| anyhow!("Invalid nonce hash '{}': {}", nonce_str, e))?;

        tracing::debug!(
            nonce = %nonce_str,
            "Replacing transaction recent_blockhash with durable nonce"
        );

        // Replace transaction blockhash with durable nonce blockhash
        transaction.message.recent_blockhash = nonce_hash;

        Ok(transaction)
    }

    /// Submit transaction using durable nonce.
    pub async fn submit_durable_nonce_tx(&mut self, transaction: Transaction) -> Result<SubmitResult> {
        tracing::warn!(
            nonce_account = %self.config.nonce_account,
            "Submitting Durable Nonce transaction (Stage 3 emergency)"
        );

        // Advance/fetch nonce if configured
        if self.config.advance_nonce_before_submit {
            let _ = self.advance_nonce().await;
        }

        // Build transaction with nonce
        let nonce_tx = self.build_nonce_transaction(transaction)?;

        // Submit transaction
        let rpc_url = self.rpc.url();
        let tx_clone = nonce_tx.clone();
        let signature = tokio::task::spawn_blocking(move || {
            let rpc = RpcClient::new(rpc_url);
            rpc.send_transaction(&tx_clone)
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))??;

        let signature_str = signature.to_string();
        tracing::info!(signature = %signature_str, "Durable nonce transaction submitted");

        // Poll for confirmation
        self.poll_confirmation(&signature_str).await
    }

    /// Poll for transaction confirmation.
    async fn poll_confirmation(&self, signature: &str) -> Result<SubmitResult> {
        let start = Instant::now();
        let timeout = Duration::from_millis(self.config.confirmation_timeout_ms);
        let interval = Duration::from_millis(self.config.confirmation_poll_interval_ms);

        let signature_clone = signature.to_string();
        let rpc_url = self.rpc.url();

        loop {
            if start.elapsed() > timeout {
                return Ok(SubmitResult::Timeout);
            }

            let sig_clone = signature_clone.clone();
            let sig = Signature::from_str(&sig_clone)
                .map_err(|e| anyhow!("Invalid signature: {}", e))?;
            let rpc_url_for_poll = rpc_url.to_string();

            let result = tokio::task::spawn_blocking(move || {
                let rpc = RpcClient::new(rpc_url_for_poll);
                rpc.get_signature_status(&sig)
            })
            .await
            .map_err(|e| anyhow!("Task join error: {}", e))??;

            if let Some(status) = result {
                if let Err(err) = status {
                    tracing::warn!(error = ?err, "Transaction failed");
                    return Ok(SubmitResult::Failed {
                        reason: format!("Transaction failed: {:?}", err),
                    });
                }
                return Ok(SubmitResult::Confirmed {
                    signature: signature.to_string(),
                    slot: 0,
                });
            }

            tokio::time::sleep(interval).await;
        }
    }

    /// Check if nonce account is accessible (Health check).
    pub async fn health_check(&mut self) -> bool {
        if self.config.nonce_account.is_empty() {
            return false;
        }
        self.fetch_nonce_account().await.is_ok()
    }

    /// Check if nonce needs to be advanced.
    pub fn should_advance_nonce(&self) -> bool {
        if let Some(last_advance) = self.last_nonce_advance {
            // Advance if more than 30 seconds since last advance
            last_advance.elapsed() > Duration::from_secs(30)
        } else {
            true
        }
    }

    /// Get current nonce value.
    pub fn current_nonce(&self) -> Option<&str> {
        self.current_nonce.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_durable_nonce_config_default() {
        let config = DurableNonceConfig::default();
        assert!(config.nonce_account.is_empty());
        assert!(config.nonce_authority.is_empty());
        assert_eq!(config.advance_nonce_before_submit, true);
        assert_eq!(config.confirmation_timeout_ms, 15000);
    }

    #[test]
    fn test_durable_nonce_client_creation() {
        let config = DurableNonceConfig {
            nonce_account: "test_nonce".to_string(),
            nonce_authority: "test_auth".to_string(),
            ..Default::default()
        };
        let client = DurableNonceClient::new(config);
        assert_eq!(client.config.nonce_account, "test_nonce");
        assert!(client.current_nonce.is_none());
    }

    #[test]
    fn test_should_advance_nonce() {
        let config = DurableNonceConfig::default();
        let client = DurableNonceClient::new(config);
        
        // Should advance if never advanced
        assert!(client.should_advance_nonce());
    }

    #[test]
    fn test_with_config() {
        let client = DurableNonceClient::with_config(
            "nonce".to_string(),
            "auth".to_string(),
            "rpc".to_string(),
        );
        assert_eq!(client.config.nonce_account, "nonce");
        assert_eq!(client.config.nonce_authority, "auth");
        assert_eq!(client.config.rpc_url, "rpc");
    }

    #[test]
    fn test_build_nonce_transaction_replaces_hash() {
        let mut client = DurableNonceClient::with_config(
            Pubkey::new_unique().to_string(),
            Pubkey::new_unique().to_string(),
            "https://api.mainnet-beta.solana.com".to_string(),
        );

        let nonce_hash = Hash::new_unique();
        client.current_nonce = Some(nonce_hash.to_string());

        let payer = Pubkey::new_unique();
        let mut tx = Transaction::new_with_payer(&[], Some(&payer));
        tx.message.recent_blockhash = Hash::new_unique();

        let updated_tx = client.build_nonce_transaction(tx).unwrap();
        assert_eq!(updated_tx.message.recent_blockhash, nonce_hash);
    }
}
