use sqlx::{SqlitePool, Transaction, Sqlite};
use anyhow::Result;

pub struct SequenceLedger {
    pool: SqlitePool,
}

impl SequenceLedger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get_last_seq(&self, context_key: &str) -> Result<u64> {
        let row = sqlx::query!(
            r#"
            SELECT last_ipc_seq FROM ipc_sequence_ledger WHERE context_key = ?
            "#,
            context_key
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(record) => Ok(record.last_ipc_seq as u64),
            None => Ok(0),
        }
    }

    pub async fn update_seq_tx<'a>(&self, tx: &mut Transaction<'a, Sqlite>, context_key: &str, new_seq: u64, timestamp: u64) -> Result<()> {
        let new_seq = new_seq as i64;
        let timestamp = timestamp as i64;
        sqlx::query!(
            r#"
            INSERT INTO ipc_sequence_ledger (context_key, last_ipc_seq, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(context_key) DO UPDATE SET
                last_ipc_seq = excluded.last_ipc_seq,
                updated_at = excluded.updated_at
            "#,
            context_key,
            new_seq,
            timestamp
        )
        .execute(&mut **tx)
        .await?;

        Ok(())
    }
}
