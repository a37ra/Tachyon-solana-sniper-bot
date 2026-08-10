use anyhow::Result;
use sqlx::SqlitePool;

pub struct WalletWatchlistRepo {
    pool: SqlitePool,
}

impl WalletWatchlistRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn is_active(&self, wallet_address: &str) -> Result<bool> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM wallet_watchlist WHERE wallet_address = ?")
                .bind(wallet_address)
                .fetch_optional(&self.pool)
                .await?;

        Ok(matches!(row.as_ref().map(|r| r.0.as_str()), Some("active")))
    }

    /// Active watchlist addresses for Geyser/logs subscription (TZ §2.5).
    pub async fn list_active(&self) -> Result<Vec<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT wallet_address FROM wallet_watchlist WHERE status = 'active'")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    pub async fn count_active(&self) -> Result<i64> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM wallet_watchlist WHERE status = 'active'")
                .fetch_one(&self.pool)
                .await?;
        Ok(row.0)
    }
}
