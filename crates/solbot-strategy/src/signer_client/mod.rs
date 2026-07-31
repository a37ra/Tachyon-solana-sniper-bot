pub mod ipc_seq_counter;

use anyhow::Result;
use solbot_common::types::intent::{SignerIntentRequest, SignerIntentResponse};

#[allow(dead_code)]
pub struct SignerClient {
    socket_path: String,
    seq_counter: ipc_seq_counter::IpcSeqCounter,
}

impl SignerClient {
    pub fn new(socket_path: String) -> Self {
        Self {
            socket_path,
            seq_counter: ipc_seq_counter::IpcSeqCounter::new(),
        }
    }

    pub async fn sign_intent(&self, mut request: SignerIntentRequest) -> Result<SignerIntentResponse> {
        let seq = self.seq_counter.next_seq(&request.context_key);
        request.ipc_seq = seq;

        tracing::info!(
            context_key = %request.context_key,
            seq = seq,
            intent_type = ?request.intent_type,
            "Sending intent to solbot-signer via gRPC over IPC socket"
        );

        // Placeholder response matching expected SignerIntentResponse
        Ok(SignerIntentResponse {
            status: solbot_common::types::intent::VerdictStatus::Accepted,
            signed_transaction_base64: Some(request.unsigned_transaction_base64.clone()),
            constructed_tx_signature: Some("dummy_signature".to_string()),
            rejection_reason: None,
            last_ipc_seq: seq,
        })
    }
}
