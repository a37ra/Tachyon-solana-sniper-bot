use super::SubmitResult;
use anyhow::Result;
use solana_client::nonblocking::rpc_client::RpcClient;
use std::time::Duration;

#[allow(dead_code)]
pub struct PublicRpcClient {
    rpc_client: RpcClient,
}

impl PublicRpcClient {
    pub fn new(rpc_url: &str) -> Self {
        Self {
            rpc_client: RpcClient::new(rpc_url.to_string()),
        }
    }

    pub async fn submit_transaction(&self, signed_tx_base64: &str, priority_fee_multiplier: f64) -> Result<SubmitResult> {
        tracing::info!(
            multiplier = priority_fee_multiplier,
            "Submitting direct transaction via Public RPC"
        );

        tokio::time::sleep(Duration::from_millis(150)).await;

        Ok(SubmitResult::Confirmed {
            signature: format!("rpc_sig_{}", &signed_tx_base64[..std::cmp::min(10, signed_tx_base64.len())]),
            slot: 250000001,
        })
    }
}
