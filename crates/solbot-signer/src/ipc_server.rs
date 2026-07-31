//! gRPC service implementation for the signer IPC protocol.
//!
//! Runs over a Unix Domain Socket. Implements the `TransactionSignerService`
//! from `signer.proto`. Each incoming intent is validated, rate-checked,
//! and if accepted — signed with the hot wallet keypair.
//!
//! Key invariants:
//! - Sequence ledger update + audit log insert are in ONE SQLite transaction (atomic)
//! - Signing only happens after all validation passes
//! - The keypair never leaves this process

use chrono::Utc;
use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::audit_log::AuditLog;
use crate::compromise_detection::CompromiseDetection;
use crate::key_material::mlock::GuardedKeypair;
use crate::sequence_ledger::SequenceLedger;
use crate::signing::sign_transaction;
use crate::validation::{validate_intent, RejectionReason};
use solbot_common::types::dex::DexProgram;
use solbot_common::types::intent::{IntentType, SignerIntentRequest};

#[allow(clippy::enum_variant_names)]
pub mod signer_proto {
    tonic::include_proto!("signer");
}

pub struct SignerService {
    ledger: Arc<SequenceLedger>,
    audit_log: Arc<AuditLog>,
    compromise_detection: Arc<CompromiseDetection>,
    keypair: Arc<GuardedKeypair>,
    pool: sqlx::SqlitePool,
}

impl SignerService {
    pub fn new(
        ledger: Arc<SequenceLedger>,
        audit_log: Arc<AuditLog>,
        compromise_detection: Arc<CompromiseDetection>,
        keypair: Arc<GuardedKeypair>,
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

    /// Convert proto IntentType enum (i32) to domain IntentType.
    fn parse_intent_type(raw: i32) -> IntentType {
        match raw {
            1 => IntentType::Buy,
            2 => IntentType::Sell,
            3 => IntentType::Emergency,
            _ => IntentType::Emergency, // conservative default
        }
    }
}

#[tonic::async_trait]
impl signer_proto::transaction_signer_service_server::TransactionSignerService for SignerService {
    async fn sign_intent(
        &self,
        request: Request<signer_proto::SignerIntentRequest>,
    ) -> Result<Response<signer_proto::SignerIntentResponse>, Status> {
        let req = request.into_inner();
        let now_ms = Utc::now().timestamp_millis() as u64;

        // Build context_key: use mint as primary key, fallback to pool_address
        let context_key = if !req.mint.is_empty() {
            req.mint.clone()
        } else {
            req.pool_address.clone()
        };

        let last_seq = self.ledger.get_last_seq(&context_key);

        // Convert proto to internal domain type
        let internal_req = SignerIntentRequest {
            context_key: context_key.clone(),
            ipc_seq: req.ipc_seq,
            issued_at_unix_ms: req.issued_at_unix_ms,
            intent_type: Self::parse_intent_type(req.intent_type),
            unsigned_transaction_base64: req.unsigned_transaction_base64.clone(),
            program_id: req.program_id.clone(),
            dex_program_id: req.dex_program_id.clone(),
            max_size_lamports: req.max_size_lamports,
            hard_cap_bps: req.hard_cap_bps,
            destination_addresses: req.destination_addresses.clone(),
            mint: req.mint.clone(),
            pool_address: req.pool_address.clone(),
            position_id: if req.position_id.is_empty() {
                None
            } else {
                Some(req.position_id.clone())
            },
            amount_or_pct: req.amount_or_pct,
        };

        // --- Validation pipeline ---

        let allowed_programs = DexProgram::all_program_ids();

        // max_size_absolute from tier config (env var or default 10 SOL)
        let max_size_absolute: u64 = std::env::var("SOLBOT_MAX_SIZE_LAMPORTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10_000_000_000); // 10 SOL default

        // hard_cap_bps limit from config (default 5000 = 50%)
        let hard_cap_bps_limit: u32 = std::env::var("SOLBOT_HARD_CAP_BPS_LIMIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5000);

        // Destination whitelist from env (comma-separated) — empty = allow all
        let dest_whitelist_str = std::env::var("SOLBOT_DEST_WHITELIST").unwrap_or_default();
        let dest_whitelist: Vec<&str> = if dest_whitelist_str.is_empty() {
            vec![]
        } else {
            dest_whitelist_str.split(',').collect()
        };

        let validation_result = validate_intent(
            &internal_req,
            last_seq,
            max_size_absolute,
            allowed_programs,
            &dest_whitelist,
            hard_cap_bps_limit,
        );
        let rate_ok = self.compromise_detection.check_signing_rate();

        // Determine verdict
        let (verdict_str, rejection_reason, signed_b64, signature) =
            match (validation_result, rate_ok) {
                (Ok(()), true) => {
                    // All checks passed → sign the transaction
                    if internal_req.unsigned_transaction_base64.is_empty() {
                        (
                            "rejected",
                            Some("MissingUnsignedTransaction"),
                            None,
                            None,
                        )
                    } else {
                        match sign_transaction(
                            &internal_req.unsigned_transaction_base64,
                            self.keypair.keypair(),
                        ) {
                            Ok((signed, sig)) => {
                                ("accepted", None, Some(signed), Some(sig))
                            }
                            Err(e) => {
                                tracing::error!(
                                    error = %e,
                                    context_key = %context_key,
                                    "Transaction signing failed"
                                );
                                (
                                    "rejected",
                                    Some("SigningFailed"),
                                    None,
                                    None,
                                )
                            }
                        }
                    }
                }
                (Err(reason), _) => (
                    "rejected",
                    Some(reason.as_str()),
                    None,
                    None,
                ),
                (_, false) => (
                    "rejected",
                    Some(RejectionReason::RateLimitExceeded.as_str()),
                    None,
                    None,
                ),
            };

        // --- Atomic persistence: sequence ledger + audit log in one SQLite tx ---
        let mut db_tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Status::internal(format!("DB transaction begin failed: {e}")))?;

        let audit_id = uuid::Uuid::new_v4().to_string();

        if verdict_str == "accepted" {
            self.ledger
                .update_seq_tx(&mut db_tx, &context_key, internal_req.ipc_seq, now_ms)
                .await
                .map_err(|e| Status::internal(format!("Sequence ledger update failed: {e}")))?;
        }

        let intent_type_str = match internal_req.intent_type {
            IntentType::Buy => "buy",
            IntentType::Sell => "sell",
            IntentType::Emergency => "emergency",
        };

        self.audit_log
            .insert_tx(
                &mut db_tx,
                &audit_id,
                &context_key,
                intent_type_str,
                internal_req.ipc_seq,
                internal_req.position_id.as_deref(),
                verdict_str,
                rejection_reason,
                signature.as_deref(),
                internal_req.issued_at_unix_ms,
                now_ms,
            )
            .await
            .map_err(|e| Status::internal(format!("Audit log insert failed: {e}")))?;

        db_tx
            .commit()
            .await
            .map_err(|e| Status::internal(format!("DB transaction commit failed: {e}")))?;

        // --- Build response ---
        let status_code = if verdict_str == "accepted" { 1 } else { 2 };

        let resp = signer_proto::SignerIntentResponse {
            status: status_code,
            rejection_reason: rejection_reason.unwrap_or("").to_string(),
            signed_transaction_base64: signed_b64.unwrap_or_default(),
            constructed_tx_signature: signature.unwrap_or_default(),
            last_ipc_seq: internal_req.ipc_seq,
        };

        if verdict_str == "accepted" {
            tracing::info!(
                context_key = %context_key,
                ipc_seq = internal_req.ipc_seq,
                intent_type = %intent_type_str,
                "Intent ACCEPTED and signed"
            );
        } else {
            tracing::warn!(
                context_key = %context_key,
                ipc_seq = internal_req.ipc_seq,
                rejection_reason = ?rejection_reason,
                "Intent REJECTED"
            );
        }

        Ok(Response::new(resp))
    }
}
