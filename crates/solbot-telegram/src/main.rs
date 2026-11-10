use anyhow::Result;
use solbot_notify::events::BotEvent;
use solbot_telegram::alerts::format_event;
use solbot_telegram::client::TelegramClient;
use solbot_telegram::email_backup::handle_critical_fallback;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::env;
use std::time::Duration;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();
    tracing::info!("Starting solbot-telegram bot service...");

    let telegram_client = TelegramClient::from_env();
    if telegram_client.is_none() {
        tracing::warn!(
            "TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID not configured — running in dry-run log mode"
        );
    }

    let (tx, mut rx) = broadcast::channel::<BotEvent>(256);

    // Non-blocking alert processing loop (never blocks trading path >1ms)
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let msg = format_event(&event);

            if let Some(ref client) = telegram_client {
                if let Err(e) = client.send_markdown_v2(&msg).await {
                    tracing::error!("Telegram alert send failed: {}", e);
                    handle_critical_fallback(&event, &msg).await;
                }
            } else {
                tracing::info!("Telegram Alert (Dry-Run):\n{}", msg);
            }
        }
    });

    // Poll SQLite state_transitions / positions table to forward real trading lifecycle alerts
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:solbot.db".to_string());
    if let Ok(pool) = SqlitePool::connect(&db_url).await {
        let tx_db = tx.clone();
        tokio::spawn(async move {
            let mut seen_transitions: HashSet<String> = HashSet::new();
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                let Ok(rows) = sqlx::query_as::<_, (String, String, String, String)>(
                    r#"
                    SELECT id, position_id, to_status, reason
                    FROM state_transitions
                    ORDER BY created_at_utc DESC
                    LIMIT 50
                    "#,
                )
                .fetch_all(&pool)
                .await
                else {
                    continue;
                };

                for (id, position_id, to_status, reason) in rows.into_iter().rev() {
                    if !seen_transitions.insert(id) {
                        continue;
                    }
                    let pos_row = sqlx::query_as::<_, (String, f64, f64, f64)>(
                        r#"
                        SELECT mint, entry_price_sol, size_sol, COALESCE(realized_pnl_net_sol, 0.0)
                        FROM positions
                        WHERE position_id = ?1
                        "#,
                    )
                    .bind(&position_id)
                    .fetch_optional(&pool)
                    .await
                    .ok()
                    .flatten();

                    let (mint, entry_price_sol, size_sol, pnl_net_sol) =
                        pos_row.unwrap_or_else(|| ("unknown".to_string(), 0.0, 0.0, 0.0));

                    let bot_event = match to_status.as_str() {
                        "open" => Some(BotEvent::PositionOpened {
                            position_id,
                            mint,
                            entry_price_sol,
                            size_sol,
                        }),
                        "tp1_filled" => Some(BotEvent::TpTriggered {
                            position_id,
                            tp_level: "TP1".to_string(),
                            sell_fraction: 0.30,
                        }),
                        "tp2_filled" => Some(BotEvent::TpTriggered {
                            position_id,
                            tp_level: "TP2".to_string(),
                            sell_fraction: 0.30,
                        }),
                        "tp3_filled" => Some(BotEvent::TpTriggered {
                            position_id,
                            tp_level: "TP3".to_string(),
                            sell_fraction: 0.20,
                        }),
                        "closed" => Some(BotEvent::PositionClosed {
                            position_id,
                            mint,
                            pnl_net_sol,
                            reason,
        position_id: "init_check".to_string(),
        mint: "SolbotTelegramInitialized".to_string(),
        entry_price_sol: 0.0,
        size_sol: 0.0,
    };
    let _ = tx.send(startup_event);

    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down solbot-telegram");
    Ok(())
}
