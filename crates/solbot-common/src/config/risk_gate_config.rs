use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskGateConfig {
    pub pre_buy_checks: PreBuyChecks,
    pub validation: ValidationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreBuyChecks {
    pub parallel_execution: bool,
    pub hard_timeout_ms: u64,
    pub checks: ChecksConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecksConfig {
    pub mint_freeze_authority: CheckTimeout,
    pub lp_lock_check: CheckTimeout,
    pub token2022_extensions: Token2022Extensions,
    pub simulate_sell: CheckTimeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckTimeout {
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token2022Extensions {
    pub timeout_ms: u64,
    pub blocklist: Vec<String>,
    pub allowlist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    pub max_size_lamports: u64,
    pub allowlist_programs: Vec<String>,
}
