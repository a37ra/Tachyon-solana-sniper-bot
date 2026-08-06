use anyhow::Result;
use solbot_common::types::position::{Position, PositionStatus};
use sqlx::SqlitePool;

pub struct PositionsRepo {
    pool: SqlitePool,
}

pub fn status_to_string(status: &PositionStatus) -> &'static str {
    match status {
        PositionStatus::Opening => "opening",
        PositionStatus::Open => "open",
        PositionStatus::Tp1Filled => "tp1_filled",
        PositionStatus::Tp2Filled => "tp2_filled",
        PositionStatus::Tp3Filled => "tp3_filled",
        PositionStatus::Closing => "closing",
        PositionStatus::Closed => "closed",
        PositionStatus::StuckManualInterventionRequired => "stuck_manual_intervention_required",
    }
}

pub fn string_to_status(s: &str) -> PositionStatus {
    match s {
        "opening" => PositionStatus::Opening,
        "open" => PositionStatus::Open,
        "tp1_filled" | "tp1filled" => PositionStatus::Tp1Filled,
        "tp2_filled" | "tp2filled" => PositionStatus::Tp2Filled,
        "tp3_filled" | "tp3filled" => PositionStatus::Tp3Filled,
        "closing" => PositionStatus::Closing,
        "closed" => PositionStatus::Closed,
        "stuck_manual_intervention_required" | "stuckmanualinterventionrequired" => {
            PositionStatus::StuckManualInterventionRequired
        }
        _ => PositionStatus::Open,
    }
}

impl PositionsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, position: &Position) -> Result<()> {
        let status_str = status_to_string(&position.status);
        let tier_str = position.tier.to_string();
        let opened_at = position.opened_at_utc as i64;
        let closed_at = position.closed_at_utc.map(|t| t as i64);

        sqlx::query(
            r#"
            INSERT INTO positions (position_id, mint, pool_address, dex_program_id, tier, status, size_sol, entry_price_sol, size_pct_of_capital, remaining_token_pct, hard_sl_price_sol, trailing_active, trailing_high_water_sol, realized_pnl_net_sol, opened_at_utc, closed_at_utc)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            "#,
        )
        .bind(&position.position_id)
        .bind(&position.mint)
        .bind(&position.pool_address)
        .bind(&position.dex_program_id)
        .bind(&tier_str)
        .bind(&status_str)
        .bind(position.size_sol)
        .bind(position.entry_price_sol)
        .bind(position.size_pct_of_capital)
        .bind(position.remaining_token_pct)
        .bind(position.hard_sl_price_sol)
        .bind(position.trailing_active)
        .bind(position.trailing_high_water_sol)
        .bind(position.realized_pnl_net_sol)
        .bind(opened_at)
        .bind(closed_at)
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
        let status_str = status_to_string(new_status);
        let closed_at = closed_at_utc.map(|t| t as i64);
        sqlx::query(
            r#"
            UPDATE positions SET status = ?1, closed_at_utc = ?2 WHERE position_id = ?3
            "#,
        )
        .bind(status_str)
        .bind(closed_at)
        .bind(position_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn update_position(&self, position: &Position) -> Result<()> {
        let status_str = status_to_string(&position.status);
        let closed_at = position.closed_at_utc.map(|t| t as i64);

        sqlx::query(
            r#"
            UPDATE positions SET 
                status = ?1,
                remaining_token_pct = ?2,
                trailing_active = ?3,
                trailing_high_water_sol = ?4,
                realized_pnl_net_sol = ?5,
                closed_at_utc = ?6
            WHERE position_id = ?7
            "#,
        )
        .bind(status_str)
        .bind(position.remaining_token_pct)
        .bind(position.trailing_active)
        .bind(position.trailing_high_water_sol)
        .bind(position.realized_pnl_net_sol)
        .bind(closed_at)
        .bind(&position.position_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_position_by_id(&self, position_id: &str) -> Result<Option<Position>> {
        let row = sqlx::query_as::<_, (String, String, String, String, String, String, f64, f64, f64, f64, f64, bool, Option<f64>, f64, i64, Option<i64>)>(
            r#"
            SELECT position_id, mint, pool_address, dex_program_id, tier, status,
                   size_sol, entry_price_sol, size_pct_of_capital, remaining_token_pct,
                   COALESCE(hard_sl_price_sol, 0.0) as hard_sl_price_sol, trailing_active, trailing_high_water_sol,
                   realized_pnl_net_sol, opened_at_utc, closed_at_utc
            FROM positions
            WHERE position_id = ?
            "#
        )
        .bind(position_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
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

                let status = string_to_status(&status);

                Ok(Some(Position {
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
                }))
            }
            None => Ok(None),
        }
    }

    pub async fn get_open_positions(&self) -> Result<Vec<Position>> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String, String, f64, f64, f64, f64, f64, bool, Option<f64>, f64, i64, Option<i64>)>(
            r#"
            SELECT position_id, mint, pool_address, dex_program_id, tier, status,
                   size_sol, entry_price_sol, size_pct_of_capital, remaining_token_pct,
                   COALESCE(hard_sl_price_sol, 0.0) as hard_sl_price_sol, trailing_active, trailing_high_water_sol,
                   realized_pnl_net_sol, opened_at_utc, closed_at_utc
            FROM positions
            WHERE status NOT IN ('closed')
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

                let status = string_to_status(&status);

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
