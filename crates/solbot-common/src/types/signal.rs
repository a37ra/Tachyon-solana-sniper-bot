use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub mint: String,
    pub pool_address: String,
    pub dex_program: String,
    pub signal_source: String,
    pub slot: u64,
    pub received_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartMoneyHit {
    pub wallet_address: String,
    pub mint: String,
    pub pool_address: String,
    pub tx_signature: String,
    pub detected_at_slot: u64,
}
