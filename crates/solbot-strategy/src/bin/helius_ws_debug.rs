//! Standalone debug binary for Helius WebSocket programSubscribe verification.
//!
//! Subscribes to all 6 target DEX programs via Helius WebSocket (programSubscribe),
//! parses each live account update using the real pool-state parsers, checks signal
//! age against `max_signal_age_slots`, and prints one line per signal.
//!
//! Does NOT call risk_gate, sizing, signer_client, or routing — this is
//! read-only verification of the WebSocket signal source.
//!
//! Environment variables:
//! - HELIUS_API_KEY (required — Helius API key)
//! - LISTEN_SECONDS  (default: 180)
//! - MAX_SIGNAL_AGE_SLOTS (default: 30)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use solana_pubkey::Pubkey;
use std::str::FromStr;
use tokio::time::timeout;

use solbot_strategy::signal::helius_ws_client::{HeliusWsClient, HeliusWsConfig};
use solbot_strategy::tx_builder::meteora_dlmm::DlmmPoolState;
use solbot_strategy::tx_builder::meteora_dynamic::DynamicAmmPoolState;
use solbot_strategy::tx_builder::pump_fun::PumpFunBondingCurve;
use solbot_strategy::tx_builder::raydium_cpmm::CpmmPoolState;

#[derive(Default, Debug)]
struct Counters {
    raydium_amm_v4: u64,
    raydium_cpmm: u64,
    raydium_clmm: u64,
    meteora_dlmm: u64,
    meteora_dynamic: u64,
    pump_fun: u64,
    unparsed: u64,
    stale_dropped: u64,
    total_received: u64,
    // Подробная статистика по программам
    meteora_dlmm_received: u64,
    meteora_dlmm_parsed: u64,
    meteora_dlmm_rejected: u64,
    meteora_dynamic_received: u64,
    meteora_dynamic_parsed: u64,
    meteora_dynamic_rejected: u64,
    pump_fun_received: u64,
    pump_fun_parsed: u64,
    pump_fun_rejected: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let api_key = std::env::var("HELIUS_API_KEY").context("HELIUS_API_KEY env var is required")?;
    let listen_secs: u64 = std::env::var("LISTEN_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(180);
    let max_signal_age_slots: u64 = std::env::var("MAX_SIGNAL_AGE_SLOTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    println!("🚀 Helius WebSocket Debug — programSubscribe verification");
    println!("========================================================");
    println!("API Key: {}...", &api_key[..8.min(api_key.len())]);
    println!("Listen duration: {}s", listen_secs);
    println!("max_signal_age_slots: {}", max_signal_age_slots);
    println!("⚠️  NO RPC calls — using cached slot from WebSocket context");
    println!();

    // Кэшируем последний известный slot из WebSocket context
    let latest_slot = Arc::new(AtomicU64::new(0));

    // All 6 target programs (same as in dex_listeners)
    let program_ids: Vec<Pubkey> = vec![
        Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8")?, // Raydium AMM v4
        Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C")?, // Raydium CP-MM
        Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK")?, // Raydium CLMM
        Pubkey::from_str("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo")?,  // Meteora DLMM
        Pubkey::from_str("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG")?,  // Meteora Dynamic AMM v2
        Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")?,  // Pump.fun
    ];

    println!("Subscribing to {} program IDs:", program_ids.len());
    for p in &program_ids {
        println!("  - {}", p);
    }
    println!();

    let helius_ws_config = HeliusWsConfig {
        ws_url: format!("wss://mainnet.helius-rpc.com/?api-key={}", api_key),
        commitment: "confirmed".to_string(),
        encoding: "base64".to_string(),
    };

    let helius_client = HeliusWsClient::new(helius_ws_config);

    let mut updates = helius_client
        .subscribe_to_programs(program_ids)
        .await
        .context("Failed to subscribe to Helius WebSocket")?;

    println!("✅ Connected. Listening for {} seconds...\n", listen_secs);

    let mut counters = Counters::default();
    let start = Instant::now();

    let result = timeout(Duration::from_secs(listen_secs), async {
        loop {
            match updates.recv().await {
                Some(update) => {
                    counters.total_received += 1;

                    // Обновляем кэш последнего слота из context события
                    let current_cached_slot = latest_slot.load(Ordering::Relaxed);
                    if update.slot > current_cached_slot {
                        latest_slot.store(update.slot, Ordering::Relaxed);
                    }
                    let current_slot = latest_slot.load(Ordering::Relaxed);

                    // slot age check (без RPC вызова!)
                    let age = current_slot.saturating_sub(update.slot);
                    if age > max_signal_age_slots {
                        counters.stale_dropped += 1;
                        println!(
                            "⏳ DROPPED (stale, age={} slots) account={} owner={} slot={}",
                            age, update.account, update.owner, update.slot
                        );
                        continue;
                    }

                    let ts = chrono_now();

                    // Логируем каждое событие на уровне WebSocket
                    println!(
                        "{} | WS_EVENT      | account={} | owner={} | slot={} | data_len={}",
                        ts, update.account, update.owner, update.slot, update.data.len()
                    );

                    // Match by owner program ID
                    let owner_str = update.owner.to_string();
                    
                    if owner_str == "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" {
                        counters.raydium_amm_v4 += 1;
                        println!(
                            "{} | RaydiumAMMv4  | account={} | slot={} | age={} | data_len={}",
                            ts, update.account, update.slot, age, update.data.len()
                        );
                    } else if owner_str == "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" {
                        match CpmmPoolState::from_account_data(&update.data) {
                            Ok(pool) => {
                                counters.raydium_cpmm += 1;
                                println!(
                                    "{} | RaydiumCPMM   | pool={} | token0={} | token1={} | slot={} | age={}",
                                    ts, update.account, pool.token_0_mint, pool.token_1_mint, update.slot, age
                                );
                            }
                            Err(e) => {
                                counters.unparsed += 1;
                                println!("{} | RaydiumCPMM   | ⚠️ parse failed: {}", ts, e);
                            }
                        }
                    } else if owner_str == "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" {
                        counters.raydium_clmm += 1;
                        println!(
                            "{} | RaydiumCLMM   | account={} | slot={} | age={} | data_len={}",
                            ts, update.account, update.slot, age, update.data.len()
                        );
                    } else if owner_str == "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo" {
                        counters.meteora_dlmm_received += 1;
                        match DlmmPoolState::from_account_data(&update.data, &update.account) {
                            Ok(pool) => {
                                counters.meteora_dlmm += 1;
                                counters.meteora_dlmm_parsed += 1;
                                println!(
                                    "{} | MeteoraDLMM   | lb_pair={} | token_x={} | token_y={} | active_id={} | slot={} | age={}",
                                    ts, update.account, pool.token_x_mint, pool.token_y_mint, pool.active_id, update.slot, age
                                );
                            }
                            Err(e) => {
                                counters.meteora_dlmm_rejected += 1;
                                counters.unparsed += 1;
                                println!("{} | MeteoraDLMM   | ⚠️ parse failed: account={} reason={}", ts, update.account, e);
                            }
                        }
                    } else if owner_str == "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG" {
                        counters.meteora_dynamic_received += 1;
                        match DynamicAmmPoolState::from_account_data(&update.data, &update.account) {
                            Ok(pool) => {
                                counters.meteora_dynamic += 1;
                                counters.meteora_dynamic_parsed += 1;
                                println!(
                                    "{} | MeteoraDynamic | pool={} | token_a={} | token_b={} | slot={} | age={}",
                                    ts, update.account, pool.token_a_mint, pool.token_b_mint, update.slot, age
                                );
                            }
                            Err(e) => {
                                counters.meteora_dynamic_rejected += 1;
                                counters.unparsed += 1;
                                println!("{} | MeteoraDynamic | ⚠️ parse failed: account={} reason={}", ts, update.account, e);
                            }
                        }
                    } else if owner_str == "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" {
                        counters.pump_fun_received += 1;
                        match PumpFunBondingCurve::from_account_data(&update.data, &update.account) {
                            Ok(pool) => {
                                counters.pump_fun += 1;
                                counters.pump_fun_parsed += 1;
                                println!(
                                    "{} | PumpFun       | bonding_curve={} | mint={} | slot={} | age={}",
                                    ts, update.account, pool.mint, update.slot, age
                                );
                            }
                            Err(e) => {
                                counters.pump_fun_rejected += 1;
                                counters.unparsed += 1;
                                println!("{} | PumpFun       | ⚠️ parse failed: account={} reason={}", ts, update.account, e);
                            }
                        }
                    } else {
                        counters.unparsed += 1;
                        println!(
                            "{} | UNPARSED      | account={} | owner={} | slot={} | data_len={}",
                            ts, update.account, update.owner, update.slot, update.data.len()
                        );
                    }
                }
                None => {
                    anyhow::bail!("WebSocket stream closed unexpectedly (reconnect logic should have kicked in)");
                }
            }
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    })
    .await;

    let elapsed = start.elapsed();

    println!("\n========================================================");
    println!("📊 Summary after {:.1}s", elapsed.as_secs_f64());
    println!("========================================================");
    println!("Total updates received : {}", counters.total_received);
    println!("  Raydium AMM v4        : {}", counters.raydium_amm_v4);
    println!("  Raydium CP-MM         : {}", counters.raydium_cpmm);
    println!("  Raydium CLMM          : {}", counters.raydium_clmm);
    println!(
        "  Meteora DLMM          : {} (received: {}, parsed: {}, rejected: {})",
        counters.meteora_dlmm,
        counters.meteora_dlmm_received,
        counters.meteora_dlmm_parsed,
        counters.meteora_dlmm_rejected
    );
    println!(
        "  Meteora Dynamic AMM   : {} (received: {}, parsed: {}, rejected: {})",
        counters.meteora_dynamic,
        counters.meteora_dynamic_received,
        counters.meteora_dynamic_parsed,
        counters.meteora_dynamic_rejected
    );
    println!(
        "  Pump.fun              : {} (received: {}, parsed: {}, rejected: {})",
        counters.pump_fun,
        counters.pump_fun_received,
        counters.pump_fun_parsed,
        counters.pump_fun_rejected
    );
    println!("  Unparsed/unknown      : {}", counters.unparsed);
    println!("  Dropped (stale)       : {}", counters.stale_dropped);
    println!(
        "  Latest cached slot    : {}",
        latest_slot.load(Ordering::Relaxed)
    );
    println!("  RPC calls made        : 0 (NO THROTTLE)");

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("\n❌ Stream error: {}", e);
            return Err(e);
        }
        Err(_) => {
            println!(
                "\n⏱️  Listen window ({}s) elapsed — this is expected, not a failure.",
                listen_secs
            );
        }
    }

    if counters.total_received == 0 {
        anyhow::bail!("No updates received at all — check HELIUS_API_KEY / endpoint / filters");
    }

    println!("\n✅ Helius WebSocket debug run complete.");
    Ok(())
}

fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_secs())
}
