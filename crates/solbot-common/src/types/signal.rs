use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub mint: String,
    pub pool_address: String,
    pub dex_program: String,
    pub signal_source: String,
    pub slot: u64,
    pub received_at: u64,
    // Optional raw pool account bytes and owner (when provided by account update sources)
    // These are source-agnostic and should be populated when available to enable
    // 1-RTT pre-buy checks (no additional RPC to fetch pool bytes).
    pub raw_pool_account_data: Option<Vec<u8>>,
    pub raw_pool_account_owner: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartMoneyHit {
    pub wallet_address: String,
    pub mint: String,
    pub pool_address: String,
    pub tx_signature: String,
    pub detected_at_slot: u64,
}
