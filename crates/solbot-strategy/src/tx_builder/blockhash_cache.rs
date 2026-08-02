//! Cached blockhash provider with automatic refresh.
//!
//! A fresh `recent_blockhash` is required for every transaction.
//! Blockhashes expire after ~60-90 seconds (~150 slots).
//!
//! This module caches the latest blockhash and refreshes it in the background
//! to avoid an RPC round-trip on every transaction build.

use anyhow::{Context, Result};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::hash::Hash;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{Duration, Instant};

/// How often to refresh the blockhash (well within the ~60s validity window).
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Maximum age before we force a synchronous refresh.
const MAX_AGE: Duration = Duration::from_secs(30);

/// Cached blockhash with metadata.
#[derive(Debug, Clone)]
struct CachedHash {
    hash: Hash,
    last_valid_block_height: u64,
    fetched_at: Instant,
}

/// Thread-safe blockhash cache with background refresh.
#[derive(Clone)]
pub struct BlockhashCache {
    rpc: Arc<RpcClient>,
    cache: Arc<RwLock<Option<CachedHash>>>,
}

impl BlockhashCache {
    /// Create a new cache and immediately fetch the first blockhash.
    pub async fn new(rpc: Arc<RpcClient>) -> Result<Self> {
        let cache = Self {
            rpc,
            cache: Arc::new(RwLock::new(None)),
        };
        cache.refresh().await?;
        Ok(cache)
    }

    /// Start the background refresh loop. Returns a JoinHandle that can be aborted on shutdown.
    pub fn start_refresh_loop(&self) -> tokio::task::JoinHandle<()> {
        let cache = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH_INTERVAL).await;
                if let Err(e) = cache.refresh().await {
                    tracing::warn!(error = %e, "Blockhash refresh failed — will retry");
                }
            }
        })
    }

    /// Get the cached blockhash. If it's too old, force a synchronous refresh.
    pub async fn get_blockhash(&self) -> Result<(Hash, u64)> {
        {
            let guard = self.cache.read().await;
            if let Some(ref cached) = *guard {
                if cached.fetched_at.elapsed() < MAX_AGE {
                    return Ok((cached.hash, cached.last_valid_block_height));
                }
            }
        }

        // Cache is stale or empty — synchronous refresh
        tracing::debug!("Blockhash cache stale, refreshing synchronously");
        self.refresh().await?;

        let guard = self.cache.read().await;
        let cached = guard.as_ref().context("Blockhash cache empty after refresh")?;
        Ok((cached.hash, cached.last_valid_block_height))
    }

    /// Get just the hash (convenience method for transaction building).
    pub async fn get_hash(&self) -> Result<Hash> {
        let (hash, _) = self.get_blockhash().await?;
        Ok(hash)
    }

    /// Fetch a fresh blockhash from RPC and update the cache.
    #[allow(clippy::result_large_err)]
    async fn refresh(&self) -> Result<()> {
        let hash = tokio::task::spawn_blocking({
            let rpc = Arc::clone(&self.rpc);
            move || rpc.get_latest_blockhash()
        })
        .await
        .context("Failed to spawn blocking task")?
        .context("Failed to fetch latest blockhash")?;

        let last_valid_block_height = 0; // TODO: Fetch from RPC if needed

        let mut guard = self.cache.write().await;
        *guard = Some(CachedHash {
            hash,
            last_valid_block_height,
            fetched_at: Instant::now(),
        });

        tracing::trace!(
            blockhash = %hash,
            last_valid_block_height,
            "Blockhash cache updated"
        );
        Ok(())
    }
}
