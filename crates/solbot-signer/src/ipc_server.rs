use tonic::{transport::Server, Request, Response, Status};
use std::sync::Arc;
use solbot_common::types::intent::{SignerIntentRequest, SignerIntentResponse, VerdictStatus};
use chrono::Utc;

use crate::sequence_ledger::SequenceLedger;
use crate::audit_log::AuditLog;
use crate::compromise_detection::CompromiseDetection;
use crate::validation::{validate_intent, RejectionReason};
use crate::signing::sign_transaction;
use solana_sdk::signature::Keypair;

pub mod signer_proto {
    tonic::include_proto!("signer");
}

use signer_proto::transaction_signer_service_server::{
    TransactionSignerService, TransactionSignerServiceServer,
};

pub struct SignerService {
    ledger: Arc<SequenceLedger>,
    audit_log: Arc<AuditLog>,
    compromise_detection: Arc<CompromiseDetection>,
    keypair: Arc<Keypair>,
    pool: sqlx::SqlitePool,
}

impl SignerService {
    pub fn new(
        ledger: Arc<SequenceLedger>,
        audit_log: Arc<AuditLog>,
        compromise_detection: Arc<CompromiseDetection>,
        keypair: Arc<Keypair>,
        pool: sqlx::SqlitePool,
    ) -> Self {
        Self {
            ledger,
            audit_log,
            compromise_detection,
            keypair,
            pool,
        }
    }
}

#[tonic::async_trait]
impl TransactionSignerService for SignerService {
    async fn sign_intent(
        &self,
        request: Request<signer_proto::SignerIntentRequest>,
    ) -> Result<Response<signer_proto::SignerIntentResponse>, Status> {
        let req = request.into_inner();
        
        let last_seq = self.ledger.get_last_seq(&req.mint); // Using mint as context_key placeholder
        
        // Convert from proto type to internal domain type
        let internal_req = SignerIntentRequest {
            context_key: req.mint.clone(),
            ipc_seq: req.ipc_seq,
            issued_at_unix_ms: req.issued_at_unix_ms as u64,
            intent_type: match req.intent_type {
                1 => solbot_common::types::intent::IntentType::Buy,
                2 => solbot_common::types::intent::IntentType::Sell,
                _ => solbot_common::types::intent::IntentType::Emergency,
            },
            unsigned_transaction_base64: "".to_string(), // In actual proto we need to pass this, but proto in TZ doesn't have it. We assume it's added.
            program_id: req.dex_program_id.clone(),
            dex_program_id: req.dex_program_id.clone(),
            max_size_lamports: (req.amount_or_pct * 1_000_000_000.0) as u64,
            hard_cap_bps: req.hard_cap_bps,
            destination_addresses: req.destination_addresses,
            mint: req.mint.clone(),
            pool_address: req.pool_address.clone(),
            position_id: Some(req.position_id.clone()),
            amount_or_pct: req.amount_or_pct,
        };

        let allowed_programs = solbot_common::types::dex::DexProgram::all_program_ids();
        let max_size = 10_000_000_000; // 10 SOL hardcode for now

        let mut tx = self.pool.begin().await.map_err(|e| Status::internal(e.to_string()))?;
        let now = Utc::now().timestamp_millis() as u64;
        let uuid = uuid::Uuid::new_v4().to_string();

        let validation_res = validate_intent(&internal_req, last_seq, max_size, allowed_programs);
        let rate_ok = self.compromise_detection.check_signing_rate();

        let (verdict, reason, signature, signed_b64) = match (validation_res, rate_ok) {
            (Ok(_), true) => {
                let (signed, sig) = ("dummy_b64".to_string(), "dummy_sig".to_string());
                (VerdictStatus::Accepted, None, Some(sig), Some(signed))
            }
            (Err(e), _) => (VerdictStatus::Rejected, Some(e.as_str()), None, None),
            (_, false) => (VerdictStatus::Rejected, Some(RejectionReason::RateLimitExceeded.as_str()), None, None),
        };

        if verdict == VerdictStatus::Accepted {
            self.ledger.update_seq_tx(&mut tx, &internal_req.context_key, internal_req.ipc_seq, now)
                .await.map_err(|e| Status::internal(e.to_string()))?;
        }

        self.audit_log.insert_tx(
            &mut tx,
            &uuid,
            &internal_req.context_key,
            "buy",
            internal_req.ipc_seq,
            internal_req.position_id.as_deref(),
            if verdict == VerdictStatus::Accepted { "accepted" } else { "rejected" },
            reason,
            signature.as_deref(),
            internal_req.issued_at_unix_ms,
            now,
        ).await.map_err(|e| Status::internal(e.to_string()))?;

        tx.commit().await.map_err(|e| Status::internal(e.to_string()))?;

        let resp = signer_proto::SignerIntentResponse {
            status: if verdict == VerdictStatus::Accepted { 1 } else { 2 },
            rejection_reason: reason.unwrap_or("").to_string(),
            signed_transaction_base64: signed_b64.unwrap_or_default(),
            constructed_tx_signature: signature.unwrap_or_default(),
            last_ipc_seq: internal_req.ipc_seq,
        };

        Ok(Response::new(resp))
    }
}
