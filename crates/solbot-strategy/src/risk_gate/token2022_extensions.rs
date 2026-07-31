use super::{CheckResult, RiskGateConfig};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::time::Duration;

const TOKEN2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

pub async fn check(mint: &str, rpc_url: &str, config: &RiskGateConfig) -> CheckResult {
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
    
    let allowlist = config.token2022_allowlist.clone();
    
    match tokio::task::spawn_blocking(move || {
        let account_info = rpc.get_account(&mint_pubkey)?;
        Ok::<_, anyhow::Error>((account_info, start.elapsed()))
    })
    .await
    {
        Ok(Ok((account_info, elapsed))) => {
            if elapsed > Duration::from_millis(25) {
                tracing::warn!("Token2022 extensions check exceeded 25ms: {}ms", elapsed.as_millis());
            }
            
            let owner = account_info.owner.to_string();
            
            if owner == TOKEN2022_PROGRAM_ID
                && !allowlist.is_empty() {
                    return CheckResult {
                        passed: false,
                        reason: Some("Token2022 not in allowlist".to_string()),
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
