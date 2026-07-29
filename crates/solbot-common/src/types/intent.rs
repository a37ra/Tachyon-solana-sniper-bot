use serde::{Deserialize, Serialize};

// This maps loosely to the protobuf definitions in `proto/signer.proto`
// but provides idiomatic Rust types for use inside `solbot-strategy`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentType {
    Buy,
    Sell,
    Emergency,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignerIntentRequest {
    pub context_key: String,
    pub ipc_seq: u64,
    pub issued_at_unix_ms: u64,
    pub intent_type: IntentType,
    pub unsigned_transaction_base64: String,
    pub program_id: String,
    pub max_size_lamports: u64,
    pub hard_cap_bps: u32,
    pub destination_addresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerdictStatus {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignerIntentResponse {
    pub status: VerdictStatus,
    pub signed_transaction_base64: Option<String>,
    pub rejection_reason: Option<String>,
    pub last_ipc_seq: u64,
}
