use anyhow::Result;
use sqlx::SqlitePool;
use std::env;

pub async fn execute(action: &str) -> Result<()> {
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:solbot.db".to_string());
    let pool_opt = SqlitePool::connect(&db_url).await.ok();

    match action {
        "status" => {
            tracing::info!("Checking circuit breaker status");
            if let Some(pool) = pool_opt {
                let start_of_day = chrono::Utc::now()
                    .date_naive()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp();
                let rows = sqlx::query_as::<_, (String, f64, i64)>(
                    r#"
                    SELECT tier,
                           COALESCE(SUM(CASE WHEN realized_pnl_net_sol < 0 THEN -realized_pnl_net_sol ELSE 0.0 END), 0.0) as daily_loss_sol,
                           COUNT(*) as closed_today
                    FROM positions
                    WHERE status = 'closed' AND COALESCE(closed_at_utc, 0) >= ?1
                    GROUP BY tier
                    "#,
        "reset" => tracing::info!("Resetting circuit breaker daily loss limits"),
        _ => tracing::info!("Unknown circuit breaker action"),
    }
    Ok(())
}
