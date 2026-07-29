use anyhow::Result;
use solbot_common::types::position::{Position, PositionStatus};
use sqlx::SqlitePool;

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
        let opened_at = position.opened_at_utc as i64;
        let closed_at = position.closed_at_utc.map(|t| t as i64);

        sqlx::query!(
            r#"
            INSERT INTO positions (position_id, mint, pool_address, dex_program_id, tier, status, size_sol, entry_price_sol, size_pct_of_capital, remaining_token_pct, hard_sl_price_sol, trailing_active, trailing_high_water_sol, realized_pnl_net_sol, opened_at_utc, closed_at_utc)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            position.position_id,
            position.mint,
            position.pool_address,
            position.dex_program_id,
            tier_str,
            status_str,
            position.size_sol,
            position.entry_price_sol,
            position.size_pct_of_capital,
            position.remaining_token_pct,
            position.hard_sl_price_sol,
            position.trailing_active,
            position.trailing_high_water_sol,
            position.realized_pnl_net_sol,
            opened_at,
            closed_at
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn update_status(
        &self,
        position_id: &str,
        new_status: &PositionStatus,
        closed_at_utc: Option<u64>,
    ) -> Result<()> {
        let status_str = format!("{:?}", new_status).to_lowercase();
        let closed_at = closed_at_utc.map(|t| t as i64);
        sqlx::query!(
            r#"
            UPDATE positions SET status = ?, closed_at_utc = ? WHERE position_id = ?
            "#,
            status_str,
            closed_at,
            position_id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
