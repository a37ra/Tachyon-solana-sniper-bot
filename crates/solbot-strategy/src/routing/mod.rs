pub mod jito_bundle;
pub mod public_rpc_fallback;
pub mod durable_nonce;

#[derive(Debug, Clone)]
pub enum SubmitResult {
    Confirmed { signature: String, slot: u64 },
    Failed { reason: String },
    Timeout,
}

#[derive(Debug, Clone)]
pub enum RoutingStrategy {
    JitoBundle,
    PublicRpcDirect,
    DurableNonce,
}
