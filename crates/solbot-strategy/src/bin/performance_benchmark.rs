//! Performance benchmark for measuring bot latency.
//!
//! Measures:
//! - RPC ping latency
//! - WebSocket connection latency
//! - Signal reception latency
//! - Log parsing latency
//! - Transaction building latency
//! - Signing latency (if signer is running)

use anyhow::Result;
use futures_util::{stream::StreamExt, SinkExt};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!("🚀 Starting Performance Benchmark");
    info!("====================================");

    // Load config from environment
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let helius_api_key = std::env::var("HELIUS_API_KEY")
        .expect("HELIUS_API_KEY must be set — see .env.example");
    let ws_url = format!("wss://mainnet.helius-rpc.com/?api-key={}", helius_api_key);

    // 1. Measure RPC ping latency
    measure_rpc_ping(&rpc_url).await;

    // 2. Measure WebSocket connection latency
    measure_ws_connection(&ws_url).await?;

    // 3. Measure WebSocket signal reception latency
    measure_ws_signal_latency(&ws_url).await?;

    // 4. Measure log parsing latency
    measure_log_parsing_latency().await;

    // 5. Measure transaction building latency
    measure_tx_building_latency().await?;

    // 6. Measure signing latency (optional, requires signer running)
    measure_signing_latency().await;

    info!("====================================");
    info!("✅ Performance Benchmark Complete");

    Ok(())
}

/// Measure RPC ping latency (getHealth)
async fn measure_rpc_ping(rpc_url: &str) {
    info!("\n📡 Measuring RPC Ping Latency");
    info!("   Endpoint: {}", rpc_url);

    let client = RpcClient::new(rpc_url.to_string());
    let mut latencies = Vec::new();

    for i in 0..10 {
        let start = Instant::now();
        match client.get_health() {
            Ok(_) => {
                let elapsed = start.elapsed();
                latencies.push(elapsed);
                info!("   Ping {}: {:?}", i + 1, elapsed);
            }
            Err(e) => {
                warn!("   Ping {} failed: {}", i + 1, e);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if !latencies.is_empty() {
        let avg = latencies.iter().sum::<Duration>() / latencies.len() as u32;
        let min = latencies.iter().min().unwrap();
        let max = latencies.iter().max().unwrap();
        info!("   Average: {:?}", avg);
        info!("   Min: {:?}", min);
        info!("   Max: {:?}", max);
    }
}

/// Measure WebSocket connection latency
async fn measure_ws_connection(ws_url: &str) -> Result<()> {
    info!("\n🔌 Measuring WebSocket Connection Latency");
    info!("   Endpoint: {}", ws_url);

    let mut latencies = Vec::new();

    for i in 0..5 {
        let start = Instant::now();
        match tokio_tungstenite::connect_async(ws_url).await {
            Ok((mut ws_stream, _)) => {
                let elapsed = start.elapsed();
                latencies.push(elapsed);
                info!("   Connection {}: {:?}", i + 1, elapsed);
                // Close the connection
                let _ = ws_stream.close(None).await;
            }
            Err(e) => {
                warn!("   Connection {} failed: {}", i + 1, e);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    if !latencies.is_empty() {
        let avg = latencies.iter().sum::<Duration>() / latencies.len() as u32;
        let min = latencies.iter().min().unwrap();
        let max = latencies.iter().max().unwrap();
        info!("   Average: {:?}", avg);
        info!("   Min: {:?}", min);
        info!("   Max: {:?}", max);
    }
    Ok(())
}

/// Measure WebSocket signal reception latency
async fn measure_ws_signal_latency(ws_url: &str) -> Result<()> {
    info!("\n📊 Measuring WebSocket Signal Reception Latency");
    info!("   Endpoint: {}", ws_url);

    let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect: {}", e))?;

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to logs for Pump.fun (most active)
    let subscribe_msg = r#"{
        "jsonrpc": "2.0",
        "id": 1,
        "method": "logsSubscribe",
        "params": [
            {
                "mentions": ["6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"]
            },
            {
                "commitment": "confirmed"
            }
        ]
    }"#;

    write
        .send(Message::Text(subscribe_msg.to_string()))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to subscribe: {}", e))?;

    info!("   Subscribed to Pump.fun logs, waiting for signals...");

    let mut latencies = Vec::new();
    let start_time = Instant::now();
    let timeout = Duration::from_secs(30);

    while latencies.len() < 10 {
        tokio::select! {
            message = read.next() => {
                match message {
                    Some(Ok(Message::Text(_text))) => {
                        let elapsed = start_time.elapsed();
                        latencies.push(elapsed);
                        info!("   Signal {}: {:?}", latencies.len(), elapsed);
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(e) = write.send(Message::Pong(payload)).await {
                            warn!("   Failed to send pong: {}", e);
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!("   WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        info!("   WebSocket closed");
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(timeout) => {
                info!("   Timeout after 30s");
                break;
            }
        }
    }

    if !latencies.is_empty() {
        let avg = latencies.iter().sum::<Duration>() / latencies.len() as u32;
        let min = latencies.iter().min().unwrap();
        let max = latencies.iter().max().unwrap();
        info!("   Average time to first signal: {:?}", avg);
        info!("   Min: {:?}", min);
        info!("   Max: {:?}", max);
    }
    Ok(())
}

/// Measure log parsing latency
async fn measure_log_parsing_latency() {
    info!("\n🔍 Measuring Log Parsing Latency");

    // Sample log message from Helius
    let sample_log = r#"{
        "jsonrpc": "2.0",
        "result": {
            "context": {"slot": 123456789},
            "value": {
                "signature": "test_signature",
                "err": null,
                "logs": [
                    "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                    "Program log: Instruction: Buy",
                    "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P consumed 12345 of 200000 compute units"
                ]
            }
        },
        "subscription": 12345
    }"#;

    let mut latencies = Vec::new();

    for i in 0..1000 {
        let start = Instant::now();
        let _ = solbot_strategy::signal::logs_parser::parse_logs_message(sample_log);
        let elapsed = start.elapsed();
        latencies.push(elapsed);

        if (i + 1) % 100 == 0 {
            info!("   Parsed {} logs...", i + 1);
        }
    }

    let avg = latencies.iter().sum::<Duration>() / latencies.len() as u32;
    let min = latencies.iter().min().unwrap();
    let max = latencies.iter().max().unwrap();
    let p50 = percentile(&latencies, 50);
    let p95 = percentile(&latencies, 95);
    let p99 = percentile(&latencies, 99);

    info!("   Average: {:?}", avg);
    info!("   Min: {:?}", min);
    info!("   Max: {:?}", max);
    info!("   P50: {:?}", p50);
    info!("   P95: {:?}", p95);
    info!("   P99: {:?}", p99);
}

/// Measure transaction building latency
async fn measure_tx_building_latency() -> Result<()> {
    info!("\n🔨 Measuring Transaction Building Latency");

    use solbot_strategy::tx_builder::{
        pump_fun::{build_swap, PumpFunBondingCurve},
        SwapDirection, SwapParams,
    };

    let wallet = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let pool_address = Pubkey::new_unique();
    let dex_program_id = Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")?;

    let params = SwapParams {
        mint,
        pool_address,
        dex_program_id,
        wallet,
        direction: SwapDirection::Buy,
        amount_in: 1_000_000_000, // 1 SOL
        minimum_amount_out: 0,
        slippage_bps: 500,
    };

    // Create a mock bonding curve state
    let bonding_curve = PumpFunBondingCurve {
        bonding_curve: pool_address,
        mint,
        associated_bonding_curve: Pubkey::new_unique(),
        virtual_token_reserves: 1_000_000_000_000,
        virtual_sol_reserves: 100_000_000_000,
        real_token_reserves: 500_000_000_000,
        real_sol_reserves: 50_000_000_000,
        token_total_supply: 1_000_000_000_000,
        complete: false,
    };

    let mut latencies = Vec::new();

    for i in 0..100 {
        let start = Instant::now();
        let _ = build_swap(&params, &bonding_curve);
        let elapsed = start.elapsed();
        latencies.push(elapsed);

        if (i + 1) % 10 == 0 {
            info!("   Built {} transactions...", i + 1);
        }
    }

    let avg = latencies.iter().sum::<Duration>() / latencies.len() as u32;
    let min = latencies.iter().min().unwrap();
    let max = latencies.iter().max().unwrap();
    let p50 = percentile(&latencies, 50);
    let p95 = percentile(&latencies, 95);

    info!("   Average: {:?}", avg);
    info!("   Min: {:?}", min);
    info!("   Max: {:?}", max);
    info!("   P50: {:?}", p50);
    info!("   P95: {:?}", p95);

    Ok(())
}

/// Measure signing latency (optional, requires signer running)
async fn measure_signing_latency() {
    info!("\n✍️  Measuring Signing Latency (Optional)");

    let socket_path = std::env::var("SOLBOT_SIGNER_SOCKET")
        .unwrap_or_else(|_| "/tmp/solbot-signer.sock".to_string());

    // Check if signer socket exists
    if !std::path::Path::new(&socket_path).exists() {
        info!("   Signer socket not found at: {}", socket_path);
        info!("   Skipping signing latency measurement");
        info!("   To enable: Start solbot-signer and set SOLBOT_SIGNER_SOCKET");
        return;
    }

    info!("   Signer socket found: {}", socket_path);
    info!("   Note: Full IPC signing measurement requires signer to be running");
    info!("   This is a placeholder - implement full gRPC client for accurate measurement");
}

/// Calculate percentile from sorted durations
fn percentile(durations: &[Duration], p: u8) -> Duration {
    let mut sorted = durations.to_vec();
    sorted.sort();
    let index = (sorted.len() as f64 * p as f64 / 100.0) as usize;
    sorted.get(index).copied().unwrap_or(Duration::ZERO)
}
