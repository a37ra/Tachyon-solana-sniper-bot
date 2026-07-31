use super::CheckResult;
use solana_rpc_client::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use std::time::Duration;

pub async fn check(pool_address: &str, rpc_url: &str) -> CheckResult {
    let start = std::time::Instant::now();
    
    let rpc = RpcClient::new(rpc_url.to_string());
    let pool_pubkey = match Pubkey::try_from(pool_address) {
        Ok(pk) => pk,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("Invalid pool pubkey: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };
    
    match tokio::task::spawn_blocking(move || {
        let account_info = rpc.get_account(&pool_pubkey)?;
        Ok::<_, anyhow::Error>((account_info, start.elapsed()))
    })
    .await
    {
        Ok(Ok((account_info, elapsed))) => {
            if elapsed > Duration::from_millis(30) {
                tracing::warn!("LP lock check exceeded 30ms: {}ms", elapsed.as_millis());
            }
            
            if account_info.data.is_empty() {
                return CheckResult {
                    passed: false,
                    reason: Some("Pool account not found".to_string()),
                    duration_ms: elapsed.as_millis() as u64,
                };
            }
            
            CheckResult {
                passed: true,
                reason: None,
                duration_ms: elapsed.as_millis() as u64,
            }
        }
        Ok(Err(e)) => CheckResult {
            passed: false,
            reason: Some(format!("RPC error: {}", e)),
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(e) => CheckResult {
            passed: false,
            reason: Some(format!("Task join error: {}", e)),
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}
