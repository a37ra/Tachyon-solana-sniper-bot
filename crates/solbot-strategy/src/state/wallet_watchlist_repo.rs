use sqlx::SqlitePool;
use anyhow::Result;

pub struct WalletWatchlistRepo {
    pool: SqlitePool,
}

impl WalletWatchlistRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn is_active(&self, wallet_address: &str) -> Result<bool> {
        let row = sqlx::query!(
            r#"
            SELECT status FROM wallet_watchlist WHERE wallet_address = ?
            "#,
            wallet_address
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(record) => Ok(record.status == "active"),
            None => Ok(false),
        }
    }
}
