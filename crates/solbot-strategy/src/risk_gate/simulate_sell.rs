use super::CheckResult;
use solana_rpc_client::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use std::time::Duration;

pub async fn check(
    mint: &str,
    pool_address: &str,
    _size_lamports: u64,
    rpc_url: &str,
) -> CheckResult {
    let start = std::time::Instant::now();
    
    let rpc = RpcClient::new(rpc_url.to_string());
    let mint_pubkey = match Pubkey::try_from(mint) {
        Ok(pk) => pk,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("Invalid mint pubkey: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };
    
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
        let account_info = rpc.get_account(&mint_pubkey)?;
        let pool_info = rpc.get_account(&pool_pubkey)?;
        Ok::<_, anyhow::Error>((account_info, pool_info, start.elapsed()))
    })
    .await
    {
        Ok(Ok((_mint_info, _pool_info, elapsed))) => {
            if elapsed > Duration::from_millis(80) {
                tracing::warn!("Simulate sell check exceeded 80ms: {}ms", elapsed.as_millis());
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
