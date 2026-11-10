#![allow(
    deprecated,
    clippy::result_large_err,
    clippy::double_must_use,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::if_same_then_else,
    clippy::collapsible_if,
    clippy::unnecessary_map_or,
    clippy::clone_on_copy,
    clippy::match_like_matches_macro,
    clippy::single_match,
    clippy::question_mark,
    clippy::needless_borrows_for_generic_args,
    clippy::unnecessary_min_or_max,
    clippy::unnecessary_cast,
    clippy::get_first,
    clippy::useless_format,
    clippy::needless_borrow
)]
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
    let client_for_alerts = telegram_client.clone();

    // Non-blocking alert processing loop (never blocks trading path >1ms)
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let msg = format_event(&event);

            if let Some(ref client) = client_for_alerts {
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
    let pool_opt = SqlitePool::connect(&db_url).await.ok();
    if let Some(ref pool) = pool_opt {
        let pool = pool.clone();
        let tx_db = tx.clone();
        tokio::spawn(async move {
            let mut seen_transitions: HashSet<String> = HashSet::new();

            // Pre-seed seen_transitions on startup so historical rows are not spammed on restart
            if let Ok(initial_rows) = sqlx::query_scalar::<_, String>(
                "SELECT id FROM state_transitions ORDER BY created_at_utc DESC LIMIT 200",
            )
            .fetch_all(&pool)
            .await
            {
                for id in initial_rows {
                    seen_transitions.insert(id);
                }
                tracing::info!(
                    seeded = seen_transitions.len(),
                    "Pre-seeded historical state_transitions to prevent startup alert spam"
                );
            }

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
                            sell_fraction: 1.0,
                        }),
                        "closed" => Some(BotEvent::PositionClosed {
                            position_id,
                            mint,
                            pnl_net_sol,
                            reason,
                        }),
                        "stuck_manual_intervention_required" => Some(BotEvent::StuckPosition {
                            position_id,
                            mint,
                            stage: reason,
                        }),
                        _ => None,
                    };

                    if let Some(ev) = bot_event {
                        let _ = tx_db.send(ev);
                    }
                }
            }
        });
    }

    // Interactive Telegram command listener (/start, /help, /status, /positions, /pnl, /kill_switch, /resume)
    if let Some(client_cmd) = telegram_client {
        let pool_cmd = pool_opt.clone();
        tokio::spawn(async move {
            let mut offset: i64 = 0;
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                let Ok(updates) = client_cmd.poll_authorized_commands(offset).await else {
                    continue;
                };
                for (update_id, text) in updates {
                    offset = update_id + 1;
                    let cmd = text
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .split('@')
                        .next()
                        .unwrap_or("");
                    let reply = handle_telegram_command(cmd, pool_cmd.as_ref()).await;
                    if let Some(reply_text) = reply {
                        let _ = client_cmd.send_plain_text(&reply_text).await;
                    }
                }
            }
        });
    }

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

async fn handle_telegram_command(cmd: &str, pool: Option<&SqlitePool>) -> Option<String> {
    let kill_switch_path = env::var("SOLBOT_KILL_SWITCH_PATH")
        .unwrap_or_else(|_| "/tmp/solbot-kill-switch".to_string());

    match cmd {
        "/start" | "/help" => Some(
            "🤖 Tachyon Solana Sniper Bot\n\n\
             Доступные команды:\n\
             /status — Статус бота и количество позиций\n\
             /positions — Список открытых позиций\n\
             /pnl — Сводка по PnL и последним сделкам\n\
             /kill_switch — Экстренная остановка бота (Kill Switch)\n\
             /resume — Снять Kill Switch\n\
             /help — Справка"
                .to_string(),
        ),
        "/status" => {
            let kill_active = std::path::Path::new(&kill_switch_path).exists();
            let (open_cnt, closed_cnt, stuck_cnt) = if let Some(p) = pool {
                let open: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM positions WHERE status IN ('open','tp1_filled','tp2_filled','tp3_filled','opening','closing')",
                )
                .fetch_one(p)
                .await
                .unwrap_or(0);
                let closed: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM positions WHERE status = 'closed'",
                )
                .fetch_one(p)
                .await
                .unwrap_or(0);
                let stuck: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM positions WHERE status = 'stuck_manual_intervention_required'",
                )
                .fetch_one(p)
                .await
                .unwrap_or(0);
                (open, closed, stuck)
            } else {
                (0, 0, 0)
            };
            Some(format!(
                "📊 Статус Tachyon Bot:\n\
                 • Режим Kill-Switch: {}\n\
                 • Открытых позиций: {}\n\
                 • Закрытых позиций: {}\n\
                 • Застрявших (Stuck): {}",
                if kill_active {
                    "🛑 АКТИВЕН (торговля остановлена)"
                } else {
                    "🟢 РАБОТАЕТ"
                },
                open_cnt,
                closed_cnt,
                stuck_cnt
            ))
        }
        "/positions" => {
            let Some(p) = pool else {
                return Some("⚠️ База данных SQLite не подключена.".to_string());
            };
            let rows = sqlx::query_as::<_, (String, String, f64, f64, f64, String)>(
                r#"
                SELECT position_id, mint, entry_price_sol, size_sol, remaining_token_pct, status
                FROM positions
                WHERE status IN ('open', 'tp1_filled', 'tp2_filled', 'tp3_filled')
                ORDER BY opened_at_utc DESC
                LIMIT 10
                "#,
            )
            .fetch_all(p)
            .await
            .unwrap_or_default();

            if rows.is_empty() {
                return Some("📭 Сейчас нет открытых позиций.".to_string());
            }
            let mut out = String::from("📈 Открытые позиции:\n\n");
            for (pos_id, mint, entry, size, rem_pct, status) in rows {
                out.push_str(&format!(
                    "• {} ({})\n  Mint: {}\n  Вход: {:.8} SOL | Размер: {:.4} SOL | Остаток: {:.0}%\n\n",
                    pos_id,
                    status,
                    mint,
                    entry,
                    size,
                    rem_pct * 100.0
                ));
            }
            Some(out)
        }
        "/pnl" => {
            let Some(p) = pool else {
                return Some("⚠️ База данных SQLite не подключена.".to_string());
            };
            let total_pnl: f64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(realized_pnl_net_sol), 0.0) FROM positions WHERE status = 'closed'",
            )
            .fetch_one(p)
            .await
            .unwrap_or(0.0);
            let wins: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM positions WHERE status = 'closed' AND realized_pnl_net_sol > 0.0",
            )
            .fetch_one(p)
            .await
            .unwrap_or(0);
            let total_closed: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM positions WHERE status = 'closed'",
            )
            .fetch_one(p)
            .await
            .unwrap_or(0);
            let winrate = if total_closed > 0 {
                (wins as f64 / total_closed as f64) * 100.0
            } else {
                0.0
            };
            Some(format!(
                "💰 Сводка PnL:\n\
                 • Чистый PnL: {:+.6} SOL\n\
                 • Закрыто сделок: {}\n\
                 • Winrate: {:.1}% ({} побед)",
                total_pnl, total_closed, winrate, wins
            ))
        }
        "/kill_switch" => {
            if let Some(parent) = std::path::Path::new(&kill_switch_path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&kill_switch_path, "1") {
                Ok(_) => Some(format!(
                    "🛑 KILL SWITCH АКТИВИРОВАН ({})\nВсе новые покупки заблокированы.",
                    kill_switch_path
                )),
                Err(e) => Some(format!("⚠️ Ошибка активации Kill Switch: {}", e)),
            }
        }
        "/resume" => {
            let _ = std::fs::remove_file(&kill_switch_path);
            Some("🟢 Kill Switch снят. Бот продолжает работу.".to_string())
        }
        _ => None,
    }
}
