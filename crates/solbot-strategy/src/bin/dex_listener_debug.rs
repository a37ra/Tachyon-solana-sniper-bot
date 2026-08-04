//! Standalone debug binary for Stage 2 (DEX Listeners) verification.
//!
//! Subscribes to all 6 target DEX programs via Geyser gRPC, parses each
//! live account update using the real pool-state parsers, checks signal
//! age against `max_signal_age_slots`, and prints one line per signal.
//!
//! Does NOT call risk_gate, sizing, signer_client, or routing — this is
//! read-only verification of the signal source.
//!
//! Environment variables:
//! - GEYSER_ENDPOINT (default: https://mainnet.helius-rpc.com)
//! - GEYSER_X_TOKEN  (required — Helius API key)
//! - RPC_URL         (default: same as GEYSER_ENDPOINT)
//! - LISTEN_SECONDS  (default: 120)
//! - MAX_SIGNAL_AGE_SLOTS (default: 30)

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use tokio::time::timeout;

use solbot_strategy::signal::geyser_client::{GeyserClient, GeyserConfig, dex_programs};
use solbot_strategy::tx_builder::raydium_cpmm::CpmmPoolState;
use solbot_strategy::tx_builder::meteora_dlmm::DlmmPoolState;
use solbot_strategy::tx_builder::meteora_dynamic::DynamicAmmPoolState;
use solbot_strategy::tx_builder::pump_fun::PumpFunBondingCurve;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG) // keep noise down, we print our own lines
        .init();

    let geyser_endpoint = std::env::var("GEYSER_ENDPOINT")
        .unwrap_or_else(|_| "https://mainnet.helius-rpc.com".to_string());
    let geyser_x_token = std::env::var("GEYSER_X_TOKEN")
        .context("GEYSER_X_TOKEN env var is required")?;
    let rpc_url = std::env::var("RPC_URL").unwrap_or_else(|_| geyser_endpoint.clone());
    let listen_secs: u64 = std::env::var("LISTEN_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let max_signal_age_slots: u64 = std::env::var("MAX_SIGNAL_AGE_SLOTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    println!("🚀 DEX Listener Debug — Stage 2 verification");
    println!("=============================================");
    println!("Geyser endpoint: {geyser_endpoint}");
    println!("Listen duration: {listen_secs}s");
    println!("max_signal_age_slots: {max_signal_age_slots}");
    println!();

    let rpc_client = Arc::new(RpcClient::new(rpc_url));

    // All 6 target programs
    let raydium_cpmm_id = dex_programs::raydium_cp_mm();
    let raydium_clmm_id = dex_programs::raydium_clmm();
    let meteora_dynamic_id = dex_programs::meteora_dynamic_amm();

    let program_ids: Vec<Pubkey> = vec![
        dex_programs::RAYDIUM_AMM_V4,
        raydium_cpmm_id,
        raydium_clmm_id,
        dex_programs::METEORA_DLMM,
        meteora_dynamic_id,
        dex_programs::PUMP_FUN,
    ];

    println!("Subscribing to {} program IDs:", program_ids.len());
    for p in &program_ids {
        println!("  - {p}");
    }
    println!();

    let geyser_config = GeyserConfig {
        endpoint: geyser_endpoint,
        x_token: Some(geyser_x_token),
        commitment: 1, // confirmed
        backfill_rpc_url: None,
        max_backfill_slots: 150,
    };

    let geyser_client = GeyserClient::new(geyser_config);

    let mut updates = geyser_client
        .subscribe_to_programs(program_ids, None, false)
        .await
        .context("Failed to subscribe to Geyser gRPC")?;

    println!("✅ Connected. Listening for {listen_secs} seconds...\n");

    let mut counters = Counters::default();
    let start = Instant::now();

    let result = timeout(Duration::from_secs(listen_secs), async {
        loop {
            match updates.recv().await {
                Some(update) => {
                    counters.total_received += 1;

                    // slot age check
                    let current_slot = rpc_client.get_slot().unwrap_or(update.slot);
                    let age = current_slot.saturating_sub(update.slot);
                    if age > max_signal_age_slots {
                        counters.stale_dropped += 1;
                        println!(
                            "⏳ DROPPED (stale, age={age} slots) account={} owner={} slot={}",
                            update.account, update.owner, update.slot
                        );
                        continue;
                    }

                    let ts = chrono_now();

                    if update.owner == dex_programs::RAYDIUM_AMM_V4 {
                        counters.raydium_amm_v4 += 1;
                        println!(
                            "{ts} | RaydiumAMMv4  | account={} | slot={} | age={age} | data_len={}",
                            update.account, update.slot, update.data.len()
                        );
                    } else if update.owner == raydium_cpmm_id {
                        match CpmmPoolState::from_account_data(&update.data) {
                            Ok(pool) => {
                                counters.raydium_cpmm += 1;
                                println!(
                                    "{ts} | RaydiumCPMM   | pool={} | token0={} | token1={} | slot={} | age={age}",
                                    update.account, pool.token_0_mint, pool.token_1_mint, update.slot
                                );
                            }
                            Err(e) => {
                                counters.unparsed += 1;
                                println!("{ts} | RaydiumCPMM   | ⚠️ parse failed: {e}");
                            }
                        }
                    } else if update.owner == raydium_clmm_id {
                        counters.raydium_clmm += 1;
                        println!(
                            "{ts} | RaydiumCLMM   | account={} | slot={} | age={age} | data_len={}",
                            update.account, update.slot, update.data.len()
                        );
                    } else if update.owner == dex_programs::METEORA_DLMM {
                        match DlmmPoolState::from_account_data(&update.data, &update.account) {
                            Ok(pool) => {
                                counters.meteora_dlmm += 1;
                                println!(
                                    "{ts} | MeteoraDLMM   | lb_pair={} | token_x={} | token_y={} | active_id={} | slot={} | age={age}",
                                    update.account, pool.token_x_mint, pool.token_y_mint, pool.active_id, update.slot
                                );
                            }
                            Err(e) => {
                                counters.unparsed += 1;
                                println!("{ts} | MeteoraDLMM   | ⚠️ parse failed: {e}");
                            }
                        }
                    } else if update.owner == meteora_dynamic_id {
                        match DynamicAmmPoolState::from_account_data(&update.data, &update.account) {
                            Ok(pool) => {
                                counters.meteora_dynamic += 1;
                                println!(
                                    "{ts} | MeteoraDynAMM | pool={} | tokenA={} | tokenB={} | slot={} | age={age}",
                                    update.account, pool.token_a_mint, pool.token_b_mint, update.slot
                                );
                            }
                            Err(e) => {
                                counters.unparsed += 1;
                                println!("{ts} | MeteoraDynAMM | ⚠️ parse failed: {e}");
                            }
                        }
                    } else if update.owner == dex_programs::PUMP_FUN {
                        match PumpFunBondingCurve::from_account_data(&update.data, &update.account) {
                            Ok(curve) => {
                                counters.pump_fun += 1;
                                println!(
                                    "{ts} | PumpFun       | curve={} | mint={} | complete={} | slot={} | age={age}",
                                    update.account, curve.mint, curve.complete, update.slot
                                );
                            }
                            Err(e) => {
                                counters.unparsed += 1;
                                println!("{ts} | PumpFun       | ⚠️ parse failed: {e}");
                            }
                        }
                    } else {
                        counters.unparsed += 1;
                        println!(
                            "{ts} | UNKNOWN_OWNER | owner={} account={} slot={}",
                            update.owner, update.account, update.slot
                        );
                    }
                }
                None => {
                    anyhow::bail!("Geyser stream closed unexpectedly (reconnect logic should have kicked in)");
                }
            }
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    })
    .await;

    let elapsed = start.elapsed();

    println!("\n=============================================");
    println!("📊 Summary after {:.1}s", elapsed.as_secs_f64());
    println!("=============================================");
    println!("Total updates received : {}", counters.total_received);
    println!("  Raydium AMM v4        : {}", counters.raydium_amm_v4);
    println!("  Raydium CP-MM         : {}", counters.raydium_cpmm);
    println!("  Raydium CLMM          : {}", counters.raydium_clmm);
    println!("  Meteora DLMM          : {}", counters.meteora_dlmm);
    println!("  Meteora Dynamic AMM   : {}", counters.meteora_dynamic);
    println!("  Pump.fun              : {}", counters.pump_fun);
    println!("  Unparsed/unknown      : {}", counters.unparsed);
    println!("  Dropped (stale)       : {}", counters.stale_dropped);

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("\n❌ Stream error: {e}");
            return Err(e);
        }
        Err(_) => {
            println!("\n⏱️  Listen window ({listen_secs}s) elapsed — this is expected, not a failure.");
        }
    }

    if counters.total_received == 0 {
        anyhow::bail!("No updates received at all — check GEYSER_X_TOKEN / endpoint / filters");
    }

    println!("\n✅ Stage 2 debug run complete.");
    Ok(())
}

fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_secs())
}
