use sqlx::{SqlitePool, Transaction, Sqlite};
use anyhow::Result;

pub struct AuditLog {
    pool: SqlitePool,
}

impl AuditLog {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert_tx<'a>(
        &self,
        tx: &mut Transaction<'a, Sqlite>,
        id: &str,
        context_key: &str,
        intent_type: &str,
        ipc_seq: u64,
        verdict: &str,
        rejection_reason: Option<&str>,
        tx_signature: Option<&str>,
        timestamp: u64
    ) -> Result<()> {
        let ipc_seq = ipc_seq as i64;
        let timestamp = timestamp as i64;
        sqlx::query!(
            r#"
            INSERT INTO signer_audit_log (id, context_key, intent_type, ipc_seq, verdict, rejection_reason, tx_signature, timestamp)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            id,
            context_key,
            intent_type,
            ipc_seq,
            verdict,
            rejection_reason,
            tx_signature,
            timestamp
        )
        .execute(&mut **tx)
        .await?;

        Ok(())
    }
}
