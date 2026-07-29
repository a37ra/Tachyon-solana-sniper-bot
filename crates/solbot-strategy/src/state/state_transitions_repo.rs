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
        timestamp: u64,
    ) -> Result<()> {
        let timestamp = timestamp as i64;
        sqlx::query(
            r#"
            INSERT INTO state_transitions (id, position_id, from_status, to_status, reason, timestamp)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(id)
        .bind(position_id)
        .bind(from_status)
        .bind(to_status)
        .bind(reason)
        .bind(timestamp)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
