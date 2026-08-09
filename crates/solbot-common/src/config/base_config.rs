use crate::types::DexLiveReadiness;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    pub primary_url: String,
    pub fallback_urls: Vec<String>,
    pub ws_url: String,
    pub timeout_ms: u64,
    pub commitment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturesConfig {
    pub smart_money_enabled: bool,
    pub smart_money_mode: String,
    pub circuit_breaker_enabled: bool,
    pub mev_protection_enabled: bool,
    pub paper_trading: PaperTradingConfig,
    pub max_signal_age_slots: u64,
    pub auto_unwrap_wsol: bool,
    pub wsol_unwrap_threshold: f64,
    pub signal_source: String, // "geyser_grpc" or "helius_ws"
    pub dex_readiness: DexReadinessConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DexReadinessConfig {
    pub raydium_amm_v4: DexLiveReadiness,
    pub raydium_cpmm: DexLiveReadiness,
    pub meteora_dlmm: DexLiveReadiness,
}

impl Default for DexReadinessConfig {
    fn default() -> Self {
        Self {
            raydium_amm_v4: DexLiveReadiness::Ready,
            raydium_cpmm: DexLiveReadiness::Ready,
            // Single-bin pricing approximation until full bin traversal is wired.
            meteora_dlmm: DexLiveReadiness::Approximate,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperTradingConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeyserConfig {
    pub endpoint: String,
    pub x_token: String,
    pub commitment: String,
    pub backfill_rpc_url: String,
    pub max_backfill_slots: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseConfig {
    pub rpc: RpcConfig,
    pub geyser: GeyserConfig,
    pub features: FeaturesConfig,
}
