use super::tier::CapitalTier;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PositionStatus {
    Opening,
    #[default]
    Open,
    Tp1Filled,
    Tp2Filled,
    Tp3Filled,
    Closing,
    Closed,
    StuckManualInterventionRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub position_id: String,
    pub mint: String,
    pub pool_address: String,
    pub dex_program_id: String,
    pub tier: CapitalTier,
    pub status: PositionStatus,
    pub entry_price_sol: f64,
    pub size_sol: f64,
    pub size_pct_of_capital: f64,
    pub remaining_token_pct: f64,
    pub hard_sl_price_sol: f64,
    pub trailing_active: bool,
    pub trailing_high_water_sol: Option<f64>,
    pub realized_pnl_net_sol: f64,
    pub opened_at_utc: u64,
    pub closed_at_utc: Option<u64>,
}
