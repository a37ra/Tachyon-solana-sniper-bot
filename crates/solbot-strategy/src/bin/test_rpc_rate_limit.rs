use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::time::Instant;

#[tokio::main]
async fn main() {
    let rpc_url = std::env::var("SOLBOT_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    
    let rpc = RpcClient::new(rpc_url.clone());
    let test_mint = Pubkey::try_from("So11111111111111111111111111111111111111112").unwrap();
    
    println!("Testing RPC rate limiting with 20 consecutive get_account requests");
    println!("RPC URL: {}", rpc_url);
    println!("Target account: {}", test_mint);
    println!();
    
    let mut times = Vec::new();
    
    for i in 1..=20 {
        let start = Instant::now();
        let result = rpc.get_account(&test_mint);
        let elapsed = start.elapsed();
        
        match result {
            Ok(_) => {
                let elapsed_ms = elapsed.as_millis();
                times.push(elapsed_ms);
                println!("Request {}: {} ms - SUCCESS", i, elapsed_ms);
            }
            Err(e) => {
                let elapsed_ms = elapsed.as_millis();
                println!("Request {}: {} ms - ERROR: {}", i, elapsed_ms, e);
            }
        }
        
        // Small delay between requests to avoid overwhelming
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    
    println!();
    println!("=== Statistics ===");
    if !times.is_empty() {
        let min = times.iter().min().unwrap();
        let max = times.iter().max().unwrap();
        let avg: f64 = times.iter().map(|&x| x as f64).sum::<f64>() / times.len() as f64;
        
        println!("Successful requests: {}", times.len());
        println!("Min: {} ms", min);
        println!("Max: {} ms", max);
        println!("Avg: {:.2} ms", avg);
        
        // Check for progressive slowdown
        let first_half_avg: f64 = times[..10].iter().map(|&x| x as f64).sum::<f64>() / 10.0;
        let second_half_avg: f64 = times[10..].iter().map(|&x| x as f64).sum::<f64>() / 10.0;
        
        println!("First 10 avg: {:.2} ms", first_half_avg);
        println!("Last 10 avg: {:.2} ms", second_half_avg);
        
        if second_half_avg > first_half_avg * 1.5 {
            println!("⚠️  PROGRESSIVE SLOWDOWN DETECTED (last 10 > 1.5x first 10) - possible rate limiting");
        } else {
            println!("✓ No significant progressive slowdown detected");
        }
    }
}
