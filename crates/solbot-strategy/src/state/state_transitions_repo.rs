use anyhow::Result;
use sqlx::SqlitePool;

pub struct StateTransitionsRepo {
    pool: SqlitePool,
}

impl StateTransitionsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(
        &self,
        id: &str,
        position_id: &str,
        from_status: &str,
        to_status: &str,
        reason: &str,
        tx_signature: Option<&str>,
        created_at_utc: u64,
    ) -> Result<()> {
        let created_at_utc = created_at_utc as i64;
        sqlx::query(
            r#"
            INSERT INTO state_transitions (id, position_id, from_status, to_status, reason, tx_signature, created_at_utc)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(id)
        .bind(position_id)
        .bind(from_status)
        .bind(to_status)
        .bind(reason)
        .bind(tx_signature)
        .bind(created_at_utc)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
