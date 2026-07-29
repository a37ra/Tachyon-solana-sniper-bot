use anyhow::Result;
use sqlx::SqlitePool;

pub struct ScamBlacklistRepo {
    pool: SqlitePool,
}

impl ScamBlacklistRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn is_blacklisted(&self, mint: &str) -> Result<bool> {
        let row = sqlx::query!(
            r#"
            SELECT 1 as "exists!" FROM scam_blacklist WHERE mint = ?
            "#,
            mint
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.is_some())
    }
}
