use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierInfo {
    pub name: String,
    pub capital_range: CapitalRange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapitalRange {
    pub min_usd: u32,
    pub max_usd: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KellyParams {
    pub fraction_of_kelly: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveParams {
    pub enabled: bool,
    pub calibration_window_slots: u64,
    pub min_sample_size: u32,
    pub size_max_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizingParams {
    pub base_size_pct: f64,
    pub max_size_sol: f64,
    pub slippage_hard_cap_bps: u32,
    pub kelly: KellyParams,
    pub adaptive: AdaptiveParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerParams {
    pub warning_threshold_pct: f64,
    pub daily_loss_limit_pct: f64,
    pub max_parallel_emergency_sells: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionLimits {
    pub max_concurrent_positions: u32,
    pub circuit_breaker: CircuitBreakerParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotWalletParams {
    pub cap_sol: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompromiseDetection {
    pub signing_rate_threshold_5min: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmergencyExitParams {
    pub emergency_slippage_bps: u32,
    pub priority_fee_baseline: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierParams {
    pub tier: TierInfo,
    pub sizing: SizingParams,
    pub position_limits: PositionLimits,
    pub hot_wallet: HotWalletParams,
    pub compromise_detection: CompromiseDetection,
    pub emergency_exit: EmergencyExitParams,
}
