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
                       remaining_token_pct, COALESCE(realized_pnl_net_sol, 0.0), opened_at_utc, closed_at_utc
                FROM positions
                WHERE position_id = ?1
                "#,
            )
            .bind(pos_id)
            .fetch_optional(&pool)
            .await?;

            match row {
                Some((id, mint, pool_addr, tier, status, size, entry, rem, pnl, opened, closed)) => {
                    println!("Position ID:         {}", id);
                    println!("Mint:                {}", mint);
                    println!("Pool Address:        {}", pool_addr);
                    println!("Tier:                {}", tier);
                    println!("Status:              {}", status);
                    println!("Size (SOL):          {:.4}", size);
                    println!("Entry Price (SOL):   {:.8}", entry);
                    println!("Remaining Token %:   {:.1}%", rem * 100.0);
                    println!("Realized PnL (SOL):  {:.6}", pnl);
                    println!("Opened At (UTC):     {}", opened);
                    println!("Closed At (UTC):     {:?}", closed);
                }
                None => println!("Position '{}' not found", pos_id),
            }
        }
        "force-close" => {
            let Some(pos_id) = position_id else {
                bail!("--id <POSITION_ID> is required for 'position force-close'");
            };
            let now = chrono::Utc::now().timestamp();
            let old_status: Option<(String,)> =
                sqlx::query_as("SELECT status FROM positions WHERE position_id = ?1")
                    .bind(pos_id)
                    .fetch_optional(&pool)
                    .await?;

            let Some((from_status,)) = old_status else {
                println!("Position '{}' not found", pos_id);
                return Ok(());
            };

            sqlx::query(
                r#"
                UPDATE positions
                SET status = 'closed', remaining_token_pct = 0.0, closed_at_utc = ?1
                WHERE position_id = ?2
                "#,
            )
            .bind(now)
            .bind(pos_id)
            .execute(&pool)
            .await?;

            let transition_id = uuid::Uuid::new_v4().to_string();
            let _ = sqlx::query(
                r#"
                INSERT INTO state_transitions (id, position_id, from_status, to_status, reason, tx_signature, created_at_utc)
                VALUES (?1, ?2, ?3, 'closed', 'cli_force_close', NULL, ?4)
                "#,
            )
            .bind(transition_id)
            .bind(pos_id)
            .bind(from_status)
            .bind(now)
            .execute(&pool)
            .await;

            tracing::info!(position_id = %pos_id, "Force-closed position in SQLite DB");
            println!("Position '{}' force-closed.", pos_id);
        }
        _ => tracing::info!("Unknown position action: {}", action),
    }
    Ok(())
}
