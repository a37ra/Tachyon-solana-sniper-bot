use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationConfig {
    pub matrix: EscalationMatrix,
    pub durable_nonce: DurableNonceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationMatrix {
    pub stage_0: Stage0Config,
    pub stage_1: Stage1Config,
    pub stage_2: Stage2Config,
    pub stage_3: Stage3Config,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage0Config {
    pub route: String,
    pub timeout_ms: u64,
    pub retries: u32,
    pub priority_fee_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage1Config {
    pub route: String,
    pub timeout_ms: u64,
    pub retries: u32,
    pub priority_fee_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage2Config {
    pub route: String,
    pub timeout_ms: u64,
    pub retries: u32,
    pub priority_fee_multiplier: f64,
    pub max_slippage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage3Config {
    pub route: String,
    pub interval_sec: u64,
    pub max_duration_min: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableNonceConfig {
    pub address: String,
    pub authority: String,
}
