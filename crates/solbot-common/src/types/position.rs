use serde::{Deserialize, Serialize};
use super::tier::CapitalTier;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionStatus {
    Opening,
    Open,
    Closing,
    Closed,
    StuckManualInterventionRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: String,
    pub mint: String,
    pub pool_address: String,
    pub tier: CapitalTier,
    pub status: PositionStatus,
    pub size_sol: f64,
    pub entry_price_sol: f64,
    pub realized_pnl_sol: f64,
    pub created_at: u64,
    pub updated_at: u64,
}
