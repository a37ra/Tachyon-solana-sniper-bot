use anyhow::Result;
use sqlx::SqlitePool;

pub struct SmartMoneyHitsRepo {
    pool: SqlitePool,
}

impl SmartMoneyHitsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert_hit(
        &self,
        id: &str,
        wallet_address: &str,
        mint: &str,
        pool_address: &str,
        tx_signature: &str,
        detected_at_slot: u64,
    ) -> Result<()> {
        let detected_at_slot = detected_at_slot as i64;
        sqlx::query(
            r#"
            INSERT INTO smart_money_hits (id, wallet_address, mint, pool_address, tx_signature, detected_at_slot)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(id)
        .bind(wallet_address)
        .bind(mint)
        .bind(pool_address)
        .bind(tx_signature)
        .bind(detected_at_slot)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// True if any watchlist wallet hit this mint (for hard_gate / soft_boost).
    pub async fn has_hit_for_mint(&self, mint: &str) -> Result<bool> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM smart_money_hits WHERE mint = ? LIMIT 1")
                .bind(mint)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }
}
