//! Helius logsSubscribe debug binary.
//!
//! Tests logsSubscribe for all 6 DEX programs and parses instruction-level signals.
//! This is for verifying the logs_parser implementation and understanding
//! the structure of transaction logs from each DEX.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use solana_pubkey::Pubkey;
use tokio::time::timeout;
use std::str::FromStr;

use solbot_strategy::signal::helius_ws_client::{HeliusWsClient, HeliusWsConfig};
use solbot_strategy::signal::logs_parser::{DexType, InstructionType, ParsedSignal};

#[derive(Default, Debug)]
struct LogsCounters {
    raydium_amm_v4: u64,
    raydium_cpmm: u64,
    raydium_clmm: u64,
    meteora_dlmm: u64,
    meteora_dynamic: u64,
    pump_fun: u64,
    initialize_pool: u64,
    swap: u64,
    init_bonding_curve: u64,
    migrate: u64,
    unknown_instruction: u64,
    total_logs: u64,
    total_parsed: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let api_key = std::env::var("HELIUS_API_KEY")
        .context("HELIUS_API_KEY env var is required")?;
    let listen_secs: u64 = std::env::var("LISTEN_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);

    println!("🚀 Helius logsSubscribe Debug — instruction-level signal verification");
    println!("========================================================================");
    println!("API Key: {}...", &api_key[..8.min(api_key.len())]);
    println!("Listen duration: {}s", listen_secs);
    println!();

    // All 6 target programs (same as in dex_listeners)
    let program_ids: Vec<Pubkey> = vec![
        Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8")?, // Raydium AMM v4
        Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C")?, // Raydium CP-MM
        Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK")?, // Raydium CLMM
        Pubkey::from_str("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo")?, // Meteora DLMM
        Pubkey::from_str("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG")?, // Meteora Dynamic AMM v2
        Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")?, // Pump.fun
    ];

    println!("Subscribing to {} program IDs:", program_ids.len());
    for p in &program_ids {
        println!("  - {}", p);
    }
    println!();

    let helius_ws_config = HeliusWsConfig {
        ws_url: format!("wss://mainnet.helius-rpc.com/?api-key={}", api_key),
        commitment: std::env::var("HELIUS_COMMITMENT")
            .unwrap_or_else(|_| "confirmed".to_string()),
        encoding: std::env::var("HELIUS_ENCODING")
            .unwrap_or_else(|_| "base64".to_string()),
    };

    let helius_client = HeliusWsClient::new(helius_ws_config);

    let mut logs = helius_client
        .subscribe_to_logs(program_ids)
        .await
        .context("Failed to subscribe to Helius logs")?;

    println!("✅ Connected. Listening for {} seconds...\n", listen_secs);

    let mut counters = LogsCounters::default();
    let start = Instant::now();

    let result = timeout(Duration::from_secs(listen_secs), async {
        loop {
            match logs.recv().await {
                Some(signal) => {
                    counters.total_parsed += 1;

                    // Count by DEX
                    match signal.dex {
                        DexType::RaydiumAmmV4 => counters.raydium_amm_v4 += 1,
                        DexType::RaydiumCpmm => counters.raydium_cpmm += 1,
                        DexType::RaydiumClmm => counters.raydium_clmm += 1,
                        DexType::MeteoraDlmm => counters.meteora_dlmm += 1,
                        DexType::MeteoraDynamic => counters.meteora_dynamic += 1,
                        DexType::PumpFun => counters.pump_fun += 1,
                    }

                    // Count by instruction type
                    match signal.instruction_type {
                        InstructionType::InitializePool => counters.initialize_pool += 1,
                        InstructionType::Swap => counters.swap += 1,
                        InstructionType::InitBondingCurve => counters.init_bonding_curve += 1,
                        InstructionType::Migrate => counters.migrate += 1,
                        InstructionType::Unknown => counters.unknown_instruction += 1,
                        _ => {}
                    }

                    // Print detailed signal info for first 10 events of each type
                    let should_print = match signal.instruction_type {
                        InstructionType::InitBondingCurve => counters.init_bonding_curve <= 10,
                        InstructionType::Migrate => counters.migrate <= 10,
                        InstructionType::Swap => counters.swap <= 10,
                        InstructionType::InitializePool => counters.initialize_pool <= 10,
                        _ => false,
                    };

                    if should_print {
                        println!(
                            "📡 SIGNAL: {:?} | {:?} | slot={} | mint={:?} | pool={} | in={:?} | out={:?} | fee={:?} | sig={}",
                            signal.dex,
                            signal.instruction_type,
                            signal.slot,
                            signal.mint,
                            signal.pool_address,
                            signal.in_amount,
                            signal.out_amount,
                            signal.fee_amount,
                            signal.signature.as_ref().map(|s| &s[..8.min(s.len())]).unwrap_or("none")
                        );
                    }
                }
                None => {
                    anyhow::bail!("Logs stream closed unexpectedly (reconnect logic should have kicked in)");
                }
            }
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    }).await;

    let elapsed = start.elapsed();

    println!("\n========================================================================");
    println!("📊 Logs Summary after {:.1}s", elapsed.as_secs_f64());
    println!("========================================================================");
    println!("Total parsed signals : {}", counters.total_parsed);
    println!();
    println!("By DEX:");
    println!("  Raydium AMM v4        : {}", counters.raydium_amm_v4);
    println!("  Raydium CP-MM         : {}", counters.raydium_cpmm);
    println!("  Raydium CLMM          : {}", counters.raydium_clmm);
    println!("  Meteora DLMM          : {}", counters.meteora_dlmm);
    println!("  Meteora Dynamic AMM   : {}", counters.meteora_dynamic);
    println!("  Pump.fun              : {}", counters.pump_fun);
    println!();
    println!("By Instruction Type:");
    println!("  InitializePool        : {}", counters.initialize_pool);
    println!("  Swap                  : {}", counters.swap);
    println!("  InitBondingCurve      : {}", counters.init_bonding_curve);
    println!("  Migrate               : {}", counters.migrate);
    println!("  Unknown               : {}", counters.unknown_instruction);

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("\n❌ Stream error: {}", e);
            return Err(e);
        }
        Err(_) => {
            println!("\n⏱️  Listen window ({}s) elapsed — this is expected, not a failure.", listen_secs);
        }
    }

    if counters.total_parsed == 0 {
        anyhow::bail!("No signals parsed at all — check HELIUS_API_KEY / endpoint / logs format");
    }

    println!("\n✅ Helius logsSubscribe debug run complete.");
    Ok(())
}
