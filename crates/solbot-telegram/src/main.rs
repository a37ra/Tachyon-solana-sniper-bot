pub mod admin_commands;
pub mod alerts;
pub mod email_backup;
pub mod markdown_v2;

use solbot_notify::events::BotEvent;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("Starting solbot-telegram bot service...");

    let (tx, mut rx) = broadcast::channel::<BotEvent>(100);

    // Spawn alert loop
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let msg = alerts::format_event(&event);
            tracing::info!("Telegram notification broadcast:\n{}", msg);
        }
    });

    // Simulate event for demonstration
    let sample_event = BotEvent::PositionOpened {
        position_id: "pos_123".to_string(),
        mint: "MintABC123".to_string(),
        entry_price_sol: 1.5,
        size_sol: 0.5,
    };
    let _ = tx.send(sample_event);

    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down solbot-telegram");
    Ok(())
}
