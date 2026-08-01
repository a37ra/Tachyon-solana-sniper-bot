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

    pub async fn get_open_positions(&self) -> Result<Vec<Position>> {
        let rows = sqlx::query_as::<_, (i64, String, String, String, String, String, f64, f64, f64, f64, f64, bool, Option<f64>, f64, i64, Option<i64>)>(
            r#"
            SELECT position_id, mint, pool_address, dex_program_id, tier, status,
                   size_sol, entry_price_sol, size_pct_of_capital, remaining_token_pct,
                   COALESCE(hard_sl_price_sol, 0.0) as hard_sl_price_sol, trailing_active, trailing_high_water_sol,
                   realized_pnl_net_sol, opened_at_utc, closed_at_utc
            FROM positions
            WHERE status IN ('opening', 'open')
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        let positions = rows
            .into_iter()
            .map(|row| {
                let (position_id, mint, pool_address, dex_program_id, tier, status,
                     size_sol, entry_price_sol, size_pct_of_capital, remaining_token_pct,
                     hard_sl_price_sol, trailing_active, trailing_high_water_sol,
                     realized_pnl_net_sol, opened_at_utc, closed_at_utc) = row;

                let tier = match tier.as_str() {
                    "micro" => solbot_common::types::tier::CapitalTier::Micro,
                    "small" => solbot_common::types::tier::CapitalTier::Small,
                    "medium" => solbot_common::types::tier::CapitalTier::Medium,
                    "large" => solbot_common::types::tier::CapitalTier::Large,
                    _ => solbot_common::types::tier::CapitalTier::Micro,
                };

                let status = match status.as_str() {
                    "opening" => PositionStatus::Opening,
                    "open" => PositionStatus::Open,
                    "tp1_filled" => PositionStatus::Tp1Filled,
                    "tp2_filled" => PositionStatus::Tp2Filled,
                    "tp3_filled" => PositionStatus::Tp3Filled,
                    "closing" => PositionStatus::Closing,
                    "closed" => PositionStatus::Closed,
                    "stuck_manual_intervention_required" => PositionStatus::StuckManualInterventionRequired,
                    _ => PositionStatus::Open,
                };

                Position {
                    position_id: position_id.to_string(),
                    mint,
                    pool_address,
                    dex_program_id,
                    tier,
                    status,
                    entry_price_sol,
                    size_sol,
                    size_pct_of_capital,
                    remaining_token_pct,
                    hard_sl_price_sol,
                    trailing_active,
                    trailing_high_water_sol,
                    realized_pnl_net_sol,
                    opened_at_utc: opened_at_utc as u64,
                    closed_at_utc: closed_at_utc.map(|v| v as u64),
                }
            })
            .collect();

        Ok(positions)
    }
}
