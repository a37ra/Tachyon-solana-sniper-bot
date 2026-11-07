use anyhow::{bail, Result};
use sqlx::SqlitePool;
use std::env;

async fn connect_db() -> Result<SqlitePool> {
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:solbot.db".to_string());
    Ok(SqlitePool::connect(&db_url).await?)
}

pub async fn execute(action: &str, position_id: Option<&str>) -> Result<()> {
    let pool = match connect_db().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Could not connect to SQLite DB ({}); running dry-run output", e);
            return Ok(());
        }
    };

    match action {
        "list" => {
            let rows = sqlx::query_as::<_, (String, String, String, String, f64, f64, f64)>(
                r#"
                SELECT position_id, mint, tier, status, size_sol, entry_price_sol, COALESCE(realized_pnl_net_sol, 0.0)
                FROM positions
                ORDER BY opened_at_utc DESC
                "#,
            )
            .fetch_all(&pool)
            .await
            .unwrap_or_default();

            println!(
                "{:<36} | {:<44} | {:<6} | {:<12} | {:>8} | {:>12} | {:>10}",
                "POSITION_ID", "MINT", "TIER", "STATUS", "SIZE_SOL", "ENTRY_SOL", "PNL_SOL"
            );
            println!("{}", "-".repeat(142));
            for (pos_id, mint, tier, status, size_sol, entry_sol, pnl_sol) in rows {
                println!(
                    "{:<36} | {:<44} | {:<6} | {:<12} | {:>8.4} | {:>12.8} | {:>10.4}",
                    pos_id, mint, tier, status, size_sol, entry_sol, pnl_sol
                );
            }
        }
        "show" => {
            let Some(pos_id) = position_id else {
                bail!("--id <POSITION_ID> is required for 'position show'");
            };
            let row = sqlx::query_as::<
                _,
                (
                    String,
                    String,
                    String,
                    String,
                    String,
                    f64,
                    f64,
                    f64,
                    f64,
                    i64,
                    Option<i64>,
                ),
            >(
                r#"
                SELECT position_id, mint, pool_address, tier, status, size_sol, entry_price_sol,
        "show" => tracing::info!("Showing details for position: {:?}", position_id),
        "force-close" => tracing::info!("Force closing position: {:?}", position_id),
        _ => tracing::info!("Unknown position action"),
    }
    Ok(())
}
