use super::SubmitResult;
use anyhow::Result;
use std::time::Duration;

pub struct JitoClient {
    endpoint: String,
    timeout_ms: u64,
}

impl JitoClient {
    pub fn new(endpoint: String, timeout_ms: u64) -> Self {
        Self { endpoint, timeout_ms }
    }

    pub async fn submit_bundle(&self, signed_tx_base64: &str, tip_lamports: u64) -> Result<SubmitResult> {
        tracing::info!(
            endpoint = %self.endpoint,
            tip = tip_lamports,
            "Submitting Jito bundle"
        );

        // Jito submission logic wrapper / placeholder
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok(SubmitResult::Confirmed {
            signature: format!("jito_sig_{}", &signed_tx_base64[..std::cmp::min(10, signed_tx_base64.len())]),
            slot: 250000000,
        })
    }
}
