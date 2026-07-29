pub mod mint_freeze_authority;
pub mod lp_lock_check;
pub mod token2022_extensions;
pub mod simulate_sell;

use solbot_common::types::Signal;

#[derive(Debug, Clone)]
pub enum RiskGateVerdict {
    Accept,
    Reject { reason: String },
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub passed: bool,
    pub reason: Option<String>,
    pub duration_ms: u64,
}

pub async fn run_pre_buy_checks(
    signal: &Signal,
    config: &RiskGateConfig,
) -> RiskGateVerdict {
    let start = std::time::Instant::now();
    
    tracing::info!("Running pre-buy checks for mint: {}", signal.mint);
    
    let (freeze_result, lp_result, token2022_result, simulate_result) = tokio::join!(
        mint_freeze_authority::check(&signal.mint, &config.rpc_url),
        lp_lock_check::check(&signal.pool_address, &config.rpc_url),
        token2022_extensions::check(&signal.mint, &config.rpc_url, &config),
        simulate_sell::check(&signal.mint, &signal.pool_address, config.simulate_size_lamports, &config.rpc_url)
    );
    
    let elapsed = start.elapsed();
    tracing::info!("Pre-buy checks completed in {}ms", elapsed.as_millis());
    
    if !freeze_result.passed {
        return RiskGateVerdict::Reject {
            reason: freeze_result.reason.unwrap_or_else(|| "Mint freeze authority check failed".to_string()),
        };
    }
    
    if !lp_result.passed {
        return RiskGateVerdict::Reject {
            reason: lp_result.reason.unwrap_or_else(|| "LP lock check failed".to_string()),
        };
    }
    
    if !token2022_result.passed {
        return RiskGateVerdict::Reject {
            reason: token2022_result.reason.unwrap_or_else(|| "Token2022 extensions check failed".to_string()),
        };
    }
    
    if !simulate_result.passed {
        return RiskGateVerdict::Reject {
            reason: simulate_result.reason.unwrap_or_else(|| "Simulate sell check failed".to_string()),
        };
    }
    
    RiskGateVerdict::Accept
}

#[derive(Debug, Clone)]
pub struct RiskGateConfig {
    pub rpc_url: String,
    pub mint_freeze_timeout_ms: u64,
    pub lp_lock_timeout_ms: u64,
    pub token2022_timeout_ms: u64,
    pub simulate_sell_timeout_ms: u64,
    pub simulate_size_lamports: u64,
    pub token2022_blocklist: Vec<String>,
    pub token2022_allowlist: Vec<String>,
}

impl Default for RiskGateConfig {
    fn default() -> Self {
        Self {
            rpc_url: "https://api.mainnet-beta.solana.com".to_string(),
            mint_freeze_timeout_ms: 20,
            lp_lock_timeout_ms: 30,
            token2022_timeout_ms: 25,
            simulate_sell_timeout_ms: 80,
            simulate_size_lamports: 1_000_000_000, // 1 SOL
            token2022_blocklist: vec![
                "transferFee".to_string(),
                "permanentDelegate".to_string(),
                "transferHook".to_string(),
            ],
            token2022_allowlist: vec![],
        }
    }
}
