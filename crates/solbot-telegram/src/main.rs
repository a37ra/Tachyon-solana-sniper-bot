use anyhow::Result;
use solbot_notify::events::BotEvent;
use solbot_telegram::alerts::format_event;
use solbot_telegram::client::TelegramClient;
use solbot_telegram::email_backup::handle_critical_fallback;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();
    tracing::info!("Starting solbot-telegram bot service...");

    let telegram_client = TelegramClient::from_env();
    if telegram_client.is_none() {
        tracing::warn!("TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID not configured — running in dry-run log mode");
    }

    let (tx, mut rx) = broadcast::channel::<BotEvent>(256);

    // Non-blocking alert processing loop (never blocks trading path >1ms)
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let msg = format_event(&event);

            if let Some(ref client) = telegram_client {
                if let Err(e) = client.send_markdown_v2(&msg).await {
                    tracing::error!("Telegram alert send failed: {}", e);
                    // Critical alert SMTP fallback on Telegram failure
                    handle_critical_fallback(&event, &msg).await;
                }
            } else {
                tracing::info!("Telegram Alert (Dry-Run):\n{}", msg);
            }
        }
    });

    // Sample health status event on service startup
    let startup_event = BotEvent::PositionOpened {
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
