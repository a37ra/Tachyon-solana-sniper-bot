pub mod ipc_seq_counter;

use anyhow::{bail, Context, Result};
use solbot_common::types::intent::{IntentType, SignerIntentRequest, SignerIntentResponse, VerdictStatus};
use std::time::Duration;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// Generated gRPC client from signer.proto
pub mod signer_proto {
    tonic::include_proto!("signer");
}

use signer_proto::transaction_signer_service_client::TransactionSignerServiceClient;

/// Default timeout for gRPC calls to the signer.
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

/// Maximum number of retry attempts on transient errors.
const MAX_RETRIES: u32 = 3;

/// Delay between retry attempts.
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// Real gRPC client for the signer IPC service over Unix Domain Socket.
///
/// Connects to `solbot-signer` via UDS and sends intent requests
/// for validation and signing. This replaces the previous placeholder
/// that returned dummy responses.
pub struct SignerClient {
    socket_path: String,
    seq_counter: ipc_seq_counter::IpcSeqCounter,
    /// Cached gRPC channel — lazily initialized on first call.
    channel: tokio::sync::OnceCell<Channel>,
    /// Timeout for each RPC call.
    timeout: Duration,
}

impl SignerClient {
    pub fn new(socket_path: String) -> Self {
        Self {
            socket_path,
            seq_counter: ipc_seq_counter::IpcSeqCounter::new(),
            channel: tokio::sync::OnceCell::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Create a client with a custom timeout.
    #[allow(dead_code)]
    pub fn with_timeout(socket_path: String, timeout: Duration) -> Self {
        Self {
            socket_path,
            seq_counter: ipc_seq_counter::IpcSeqCounter::new(),
            channel: tokio::sync::OnceCell::new(),
            timeout,
        }
    }

    /// Get or create the gRPC channel over UDS.
    async fn get_channel(&self) -> Result<Channel> {
        let channel = self.channel.get_or_try_init(|| async {
            let socket_path = self.socket_path.clone();

            // Endpoint URI is ignored for UDS — the connector overrides it.
            // We use a dummy HTTP URI because tonic requires one.
            let channel = Endpoint::try_from("http://[::]:50051")
                .context("Failed to create endpoint")?
                .connect_timeout(Duration::from_secs(5))
                .timeout(self.timeout)
                .connect_with_connector(service_fn(move |_: Uri| {
                    let path = socket_path.clone();
                    async move {
                        let stream = UnixStream::connect(path).await?;
                        Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                    }
                }))
                .await
                .context("Failed to connect to solbot-signer via UDS")?;

            Ok::<Channel, anyhow::Error>(channel)
        }).await?;

        Ok(channel.clone())
    }

    /// Send a signing intent to solbot-signer via gRPC over UDS.
    ///
    /// Automatically assigns the next IPC sequence number for anti-replay.
    /// Retries on transient transport errors (up to MAX_RETRIES).
    pub async fn sign_intent(&self, mut request: SignerIntentRequest) -> Result<SignerIntentResponse> {
        let seq = self.seq_counter.next_seq(&request.context_key);
        request.ipc_seq = seq;

        tracing::info!(
            context_key = %request.context_key,
            seq = seq,
            intent_type = ?request.intent_type,
            "Sending intent to solbot-signer via gRPC over UDS"
        );

        // Convert domain type to proto
        let proto_req = signer_proto::SignerIntentRequest {
            context_key: request.context_key.clone(),
            ipc_seq: request.ipc_seq,
            issued_at_unix_ms: request.issued_at_unix_ms,
            intent_type: match request.intent_type {
                IntentType::Buy => 1,
                IntentType::Sell => 2,
                IntentType::Emergency => 3,
            },
            unsigned_transaction_base64: request.unsigned_transaction_base64.clone(),
            program_id: request.program_id.clone(),
            dex_program_id: request.dex_program_id.clone(),
            max_size_lamports: request.max_size_lamports,
            hard_cap_bps: request.hard_cap_bps,
            destination_addresses: request.destination_addresses.clone(),
            mint: request.mint.clone(),
            pool_address: request.pool_address.clone(),
            position_id: request.position_id.clone().unwrap_or_default(),
            amount_or_pct: request.amount_or_pct,
        };

        // Retry loop for transient errors
        let mut last_err = None;
        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                tracing::warn!(
                    attempt = attempt,
                    context_key = %request.context_key,
                    "Retrying signer RPC call"
                );
                tokio::time::sleep(RETRY_DELAY).await;
            }

            match self.do_sign_intent(proto_req.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    tracing::warn!(
                        attempt = attempt,
                        error = %e,
                        context_key = %request.context_key,
                        "Signer RPC call failed"
                    );
                    last_err = Some(e);
                }
            }
        }

        bail!(
            "Signer RPC failed after {} retries: {}",
            MAX_RETRIES,
            last_err.unwrap()
        )
    }

    /// Execute a single RPC call to the signer.
    async fn do_sign_intent(
        &self,
        proto_req: signer_proto::SignerIntentRequest,
    ) -> Result<SignerIntentResponse> {
        let channel = self.get_channel().await?;
        let mut client = TransactionSignerServiceClient::new(channel);

        let response = client
            .sign_intent(tonic::Request::new(proto_req))
            .await
            .context("gRPC SignIntent call failed")?;

        let resp = response.into_inner();

        // Convert proto response to domain type
        let status = match resp.status {
            1 => VerdictStatus::Accepted,
            _ => VerdictStatus::Rejected,
        };

        let result = SignerIntentResponse {
            status: status.clone(),
            signed_transaction_base64: if resp.signed_transaction_base64.is_empty() {
                None
            } else {
                Some(resp.signed_transaction_base64)
            },
            constructed_tx_signature: if resp.constructed_tx_signature.is_empty() {
                None
            } else {
                Some(resp.constructed_tx_signature)
            },
            rejection_reason: if resp.rejection_reason.is_empty() {
                None
            } else {
                Some(resp.rejection_reason.clone())
            },
            last_ipc_seq: resp.last_ipc_seq,
        };

        match &status {
            VerdictStatus::Accepted => {
                tracing::info!(
                    last_ipc_seq = resp.last_ipc_seq,
                    "Signer ACCEPTED intent"
                );
            }
            VerdictStatus::Rejected => {
                tracing::warn!(
                    rejection_reason = %resp.rejection_reason,
                    last_ipc_seq = resp.last_ipc_seq,
                    "Signer REJECTED intent"
                );
            }
        }

        Ok(result)
    }

    /// Check if the signer is reachable (health check).
    #[allow(dead_code)]
    pub async fn is_healthy(&self) -> bool {
        self.get_channel().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = SignerClient::new("/tmp/solbot-signer.sock".to_string());
        assert_eq!(client.socket_path, "/tmp/solbot-signer.sock");
    }

    #[test]
    fn test_client_with_custom_timeout() {
        let client = SignerClient::with_timeout(
            "/tmp/solbot-signer.sock".to_string(),
            Duration::from_secs(2),
        );
        assert_eq!(client.timeout, Duration::from_secs(2));
    }

    #[test]
    fn test_seq_counter_integration() {
        let client = SignerClient::new("/tmp/test.sock".to_string());
        let seq1 = client.seq_counter.next_seq("mint_a");
        let seq2 = client.seq_counter.next_seq("mint_a");
        let seq3 = client.seq_counter.next_seq("mint_b");
        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);
        assert_eq!(seq3, 1); // different context_key starts at 1
    }
}
