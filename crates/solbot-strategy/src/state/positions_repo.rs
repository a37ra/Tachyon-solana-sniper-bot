use sqlx::SqlitePool;
use anyhow::Result;
use solbot_common::types::position::{Position, PositionStatus};
use std::str::FromStr;

pub struct PositionsRepo {
    pool: SqlitePool,
}

impl PositionsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, position: &Position) -> Result<()> {
        let status_str = format!("{:?}", position.status).to_lowercase();
        let tier_str = position.tier.to_string();
        let created_at = position.created_at as i64;
        let updated_at = position.updated_at as i64;
        
        sqlx::query!(
            r#"
            INSERT INTO positions (id, mint, pool_address, tier, status, size_sol, entry_price_sol, realized_pnl_sol, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            position.id,
            position.mint,
            position.pool_address,
            tier_str,
            status_str,
            position.size_sol,
            position.entry_price_sol,
            position.realized_pnl_sol,
            created_at,
            updated_at
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn update_status(&self, id: &str, new_status: &PositionStatus, updated_at: u64) -> Result<()> {
        let status_str = format!("{:?}", new_status).to_lowercase();
        let updated_at = updated_at as i64;
        sqlx::query!(
            r#"
            UPDATE positions SET status = ?, updated_at = ? WHERE id = ?
            "#,
            status_str,
            updated_at,
            id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
