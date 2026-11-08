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
            "#,
        )
        .execute(pool)
        .await;
    }

    let now = chrono::Utc::now().timestamp();
    match action {
        "on" => {
            if let Some(ref pool) = pool_opt {
                let _ = sqlx::query(
                    r#"
                    INSERT INTO bot_control_state (key, value, updated_at_utc)
                    VALUES ('kill_switch', 'on', ?1)
                    ON CONFLICT(key) DO UPDATE SET value = 'on', updated_at_utc = ?1
                    "#,
                )
                .bind(now)
                .execute(pool)
                .await;
            }
            tracing::info!("Kill switch ACTIVATED via CLI");
            println!("Kill switch: ON (activated at {})", now);
        }
        "off" => {
        _ => tracing::info!("Kill switch status checked via CLI"),
    }
    Ok(())
}
