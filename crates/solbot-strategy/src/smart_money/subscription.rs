//! Load curated watchlist wallets (TZ §2.5 consumption side).

use crate::state::wallet_watchlist_repo::WalletWatchlistRepo;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory set of active watchlist addresses for O(1) hit checks.
#[derive(Debug, Default, Clone)]
pub struct WatchlistCache {
    inner: Arc<RwLock<HashSet<String>>>,
}

impl WatchlistCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn reload_from_repo(&self, repo: &WalletWatchlistRepo) -> Result<usize> {
        let addrs = repo.list_active().await?;
        let n = addrs.len();
        let mut g = self.inner.write().await;
        *g = addrs.into_iter().collect();
        tracing::info!(count = n, "smart_money watchlist cache reloaded");
        Ok(n)
    }

    pub async fn contains(&self, wallet: &str) -> bool {
        self.inner.read().await.contains(wallet)
    }

    pub async fn snapshot(&self) -> Vec<String> {
        self.inner.read().await.iter().cloned().collect()
    }

    /// Test/helper: replace set without DB.
    pub async fn replace(&self, wallets: impl IntoIterator<Item = String>) {
        let mut g = self.inner.write().await;
        *g = wallets.into_iter().collect();
    }
}

/// Addresses to subscribe on Geyser/logs (same feed as pool signals — TZ 2.5).
pub async fn subscription_wallets(cache: &WatchlistCache) -> Vec<String> {
    cache.snapshot().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_replace_and_contains() {
        let c = WatchlistCache::new();
        c.replace(["WalletA".into(), "WalletB".into()]).await;
        assert!(c.contains("WalletA").await);
        assert!(!c.contains("WalletC").await);
        assert_eq!(subscription_wallets(&c).await.len(), 2);
    }
}
