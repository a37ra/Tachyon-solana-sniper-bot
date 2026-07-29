use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevConfig {
    pub jito: JitoMevConfig,
    pub public_rpc: PublicRpcMevConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JitoMevConfig {
    pub tip_percentile: u32,
    pub min_tip_lamports: u64,
    pub max_tip_lamports: u64,
    pub jitter_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicRpcMevConfig {
    pub priority_fee_multiplier: f64,
    pub fallback_jitter_ms: JitterMs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JitterMs {
    pub min: u64,
    pub max: u64,
}
