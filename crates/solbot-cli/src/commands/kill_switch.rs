use anyhow::Result;
use sqlx::SqlitePool;
use std::env;

pub async fn execute(action: &str) -> Result<()> {
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:solbot.db".to_string());
    let pool_opt = SqlitePool::connect(&db_url).await.ok();

    if let Some(ref pool) = pool_opt {
        let _ = sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS bot_control_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at_utc INTEGER NOT NULL
            )
pub async fn execute(action: &str) -> Result<()> {
    match action {
        "on" => tracing::info!("Kill switch ACTIVATED via CLI"),
        "off" => tracing::info!("Kill switch DEACTIVATED via CLI"),
        _ => tracing::info!("Kill switch status checked via CLI"),
    }
    Ok(())
}
