use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierParams {
    pub base_size_pct: f64,
    pub hot_wallet_cap_sol: f64,
    pub signing_rate_threshold: u32,
    pub max_concurrent_positions: u32,
    pub emergency_slippage_bps: u32,
}
