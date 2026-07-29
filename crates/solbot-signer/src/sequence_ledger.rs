use anyhow::Result;
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::collections::HashMap;
use std::sync::RwLock;

pub struct SequenceLedger {
    pool: SqlitePool,
    cache: RwLock<HashMap<String, u64>>,
}

impl SequenceLedger {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            cache: RwLock::new(HashMap::new()),
        }
    }

    pub async fn hydrate_all(&self) -> Result<()> {
        let rows = sqlx::query!(
            r#"
            SELECT context_key, last_ipc_seq FROM ipc_sequence_ledger
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        let mut cache = self.cache.write().unwrap();
        for row in rows {
            cache.insert(row.context_key.unwrap_or_default(), row.last_ipc_seq as u64);
        }

        Ok(())
    }

    pub fn get_last_seq(&self, context_key: &str) -> u64 {
        let cache = self.cache.read().unwrap();
        cache.get(context_key).copied().unwrap_or(0)
    }

    pub async fn update_seq_tx<'a>(
        &self,
        tx: &mut Transaction<'a, Sqlite>,
        context_key: &str,
        new_seq: u64,
        timestamp: u64,
    ) -> Result<()> {
        let new_seq_i64 = new_seq as i64;
        let timestamp_i64 = timestamp as i64;
        sqlx::query!(
            r#"
            INSERT INTO ipc_sequence_ledger (context_key, last_ipc_seq, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(context_key) DO UPDATE SET
                last_ipc_seq = excluded.last_ipc_seq,
                updated_at = excluded.updated_at
            "#,
            context_key,
            new_seq_i64,
            timestamp_i64
        )
        .execute(&mut **tx)
        .await?;

        // Update memory cache
        let mut cache = self.cache.write().unwrap();
        cache.insert(context_key.to_string(), new_seq);

        Ok(())
    }
}
