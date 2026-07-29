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
        tx_signature: &str,
        detected_at_slot: u64,
    ) -> Result<()> {
        let detected_at_slot = detected_at_slot as i64;
        sqlx::query!(
            r#"
            INSERT INTO smart_money_hits (id, wallet_address, mint, tx_signature, detected_at_slot)
            VALUES (?, ?, ?, ?, ?)
            "#,
            id,
            wallet_address,
            mint,
            tx_signature,
            detected_at_slot
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
