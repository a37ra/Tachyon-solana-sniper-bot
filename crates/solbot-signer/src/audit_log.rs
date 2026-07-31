use anyhow::Result;
use sqlx::{Sqlite, SqlitePool, Transaction};

#[allow(dead_code)]
pub struct AuditLog {
    pool: SqlitePool,
}

impl AuditLog {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_tx<'a>(
        &self,
        tx: &mut Transaction<'a, Sqlite>,
        id: &str,
        context_key: &str,
        intent_type: &str,
        ipc_seq: u64,
        position_id: Option<&str>,
        verdict: &str,
        rejection_reason: Option<&str>,
        constructed_tx_signature: Option<&str>,
        issued_at_unix_ms: u64,
        received_at_utc: u64,
    ) -> Result<()> {
        let ipc_seq = ipc_seq as i64;
        let issued_at_unix_ms = issued_at_unix_ms as i64;
        let received_at_utc = received_at_utc as i64;
        sqlx::query!(
            r#"
            INSERT INTO signer_audit_log (
                id, context_key, intent_type, ipc_seq, position_id, 
                verdict, rejection_reason, constructed_tx_signature, 
                issued_at_unix_ms, received_at_utc
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            id,
            context_key,
            intent_type,
            ipc_seq,
            position_id,
            verdict,
            rejection_reason,
            constructed_tx_signature,
            issued_at_unix_ms,
            received_at_utc
        )
        .execute(&mut **tx)
        .await?;

        Ok(())
    }
}
