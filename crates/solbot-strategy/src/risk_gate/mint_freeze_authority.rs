use super::CheckResult;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::program_pack::Pack;
use std::time::Duration;

pub async fn check(mint: &str, rpc_url: &str) -> CheckResult {
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
    
    match tokio::task::spawn_blocking(move || {
        let account_info = rpc.get_account(&mint_pubkey)?;
        Ok::<_, anyhow::Error>((account_info, start.elapsed()))
    })
    .await
    {
        Ok(Ok((account_info, elapsed))) => {
            if elapsed > Duration::from_millis(20) {
                tracing::warn!("Mint freeze authority check exceeded 20ms: {}ms", elapsed.as_millis());
            }
            
            let mint_data = spl_token::state::Mint::unpack(&account_info.data);
            
            match mint_data {
                Ok(mint) => {
                    if mint.freeze_authority.is_some() {
                        CheckResult {
                            passed: false,
                            reason: Some("Mint has freeze authority enabled".to_string()),
                            duration_ms: elapsed.as_millis() as u64,
                        }
                    } else {
                        CheckResult {
                            passed: true,
                            reason: None,
                            duration_ms: elapsed.as_millis() as u64,
                        }
                    }
                }
                Err(e) => CheckResult {
                    passed: false,
                    reason: Some(format!("Failed to parse mint data: {}", e)),
                    duration_ms: elapsed.as_millis() as u64,
                },
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
