//! Simulate Sell Before Buy (Risk Gate 3.4).
//!
//! Builds a real sell swap transaction and simulates it via RPC to detect honeypots:
//! - Transaction reverts (custom program logic blocking sells)
//! - Excessive fees (hidden taxes on transfers)
//! - Zero output (token cannot be sold)
//!
//! Uses a hard timeout of 30ms with fallback reject to prevent
//! slow RPC responses from blocking the trading pipeline.

use super::CheckResult;
use crate::tx_builder::{
    meteora_dlmm, meteora_dynamic, pump_fun, raydium_amm_v4, raydium_clmm, raydium_cpmm,
    SwapDirection, SwapInstructions, SwapParams,
};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_transaction::Transaction;
use std::sync::Arc;
use std::time::Duration;

/// Maximum simulation time in milliseconds (aggressively optimized: 80ms → 30ms)
/// TEMP: Raised to 120ms for current remote RPC latency. This should be
/// tightened once the bot runs from a lower-latency colocated host.
const SIMULATION_TIMEOUT_MS: u64 = 120;

pub async fn check(
    mint: &str,
    pool_address: &str,
    size_lamports: u64,
    rpc: Arc<RpcClient>,
) -> CheckResult {
    let start = std::time::Instant::now();
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

    let mint_clone = mint.to_string();
    let pool_clone = pool_address.to_string();

    // Apply hard timeout (30ms) with fallback reject
    match tokio::time::timeout(
        Duration::from_millis(SIMULATION_TIMEOUT_MS),
        simulate_sell_transaction(
            rpc,
            mint_pubkey,
            pool_pubkey,
            size_lamports,
            mint_clone,
            pool_clone,
        ),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => CheckResult {
            passed: false,
            reason: Some(format!("Simulation error: {}", e)),
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(_) => {
            tracing::warn!(
                mint = %mint,
                pool = %pool_address,
                "Simulate sell check timed out after {}ms - rejecting",
                SIMULATION_TIMEOUT_MS
            );
            CheckResult {
                passed: false,
                reason: Some(format!(
                    "Simulation timeout after {}ms",
                    SIMULATION_TIMEOUT_MS
                )),
                duration_ms: SIMULATION_TIMEOUT_MS,
            }
        }
    }
}

/// Build and simulate a real sell transaction.
async fn simulate_sell_transaction(
    rpc: std::sync::Arc<solana_rpc_client::nonblocking::rpc_client::RpcClient>,
    mint_pubkey: Pubkey,
    pool_pubkey: Pubkey,
    size_lamports: u64,
    mint_str: String,
    pool_str: String,
) -> anyhow::Result<CheckResult> {
    let start = std::time::Instant::now();

    // 1. Fetch pool account and blockhash in parallel using nonblocking async
    let (pool_account_result, blockhash_result) =
        tokio::join!(rpc.get_account(&pool_pubkey), rpc.get_latest_blockhash());

    let pool_account = pool_account_result?;
    let recent_blockhash = blockhash_result?;
    let dex_program = pool_account.owner;

    // Dummy wallet for simulation
    let dummy_wallet = Pubkey::new_unique();

    let params = SwapParams {
        mint: mint_pubkey,
        pool_address: pool_pubkey,
        dex_program_id: dex_program,
        wallet: dummy_wallet,
        direction: SwapDirection::Sell,
        amount_in: size_lamports,
        minimum_amount_out: 1,
        slippage_bps: 1000, // 10%
    };

    // 2. Build sell swap instructions based on DEX type
    let dex_str = dex_program.to_string();
    let swap_instructions_result: anyhow::Result<SwapInstructions> = match dex_str.as_str() {
        "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" => {
            let state =
                raydium_amm_v4::RaydiumAmmV4PoolState::from_account_data(&pool_account.data)?;
            raydium_amm_v4::build_swap(&params, &state)
        }
        "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" => {
            let state = raydium_cpmm::CpmmPoolState::from_account_data(&pool_account.data)?;
            raydium_cpmm::build_swap(&params, &state)
        }
        "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" => {
            let state = raydium_clmm::ClmmPoolState::from_account_data(&pool_account.data)?;
            raydium_clmm::build_swap(&params, &state, &pool_pubkey, None)
        }
        "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB" => {
            let state = meteora_dynamic::DynamicAmmPoolState::from_account_data(
                &pool_account.data,
                &pool_pubkey,
            )?;
            meteora_dynamic::build_swap(&params, &state)
        }
        "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp" => {
            let state =
                meteora_dlmm::DlmmPoolState::from_account_data(&pool_account.data, &pool_pubkey)?;
            meteora_dlmm::build_swap(&params, &state)
        }
        "6EF8rrecthR5DkZ8w3bq6z3Y3Y3Y3Y3Y3Y3Y3Y3Y3Y" => {
            let curve =
                pump_fun::PumpFunBondingCurve::from_account_data(&pool_account.data, &pool_pubkey)?;
            pump_fun::build_swap(&params, &curve)
        }
        _ => anyhow::bail!("Unsupported DEX program for simulation: {}", dex_str),
    };

    // 3. Handle instruction building failure
    let swap_instructions = match swap_instructions_result {
        Ok(instructions) => instructions,
        Err(e) => {
            tracing::warn!(
                mint = %mint_str,
                pool = %pool_str,
                error = %e,
                "Failed to build sell swap instructions for simulation"
            );
            return Ok(CheckResult {
                passed: false,
                reason: Some(format!("Failed to build sell swap instructions: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
    };

    // 4. Assemble instructions into transaction
    let mut instructions = Vec::new();
    instructions.extend(swap_instructions.setup_instructions);
    instructions.extend(swap_instructions.swap_instructions);
    instructions.extend(swap_instructions.cleanup_instructions);

    let mut transaction = Transaction::new_with_payer(&instructions, Some(&dummy_wallet));
    transaction.message.recent_blockhash = recent_blockhash;

    // 5. Simulate transaction on-chain using nonblocking async
    let simulation_result = rpc.simulate_transaction(&transaction).await?;
    let elapsed = start.elapsed();

    // 6. Check for honeypot errors in simulation
    if let Some(err) = simulation_result.value.err {
        let err_str = format!("{:?}", err);
        tracing::warn!(
            mint = %mint_str,
            pool = %pool_str,
            error = %err_str,
            "Sell simulation returned error - potential honeypot"
        );

        if err_str.contains("Custom")
            || err_str.contains("ProgramFailed")
            || err_str.contains("InstructionError")
        {
            return Ok(CheckResult {
                passed: false,
                reason: Some(format!(
                    "Sell simulation failed (honeypot check): {}",
                    err_str
                )),
                duration_ms: elapsed.as_millis() as u64,
            });
        }
    }

    // 7. Check logs for fee/tax or freeze indicators
    if let Some(logs) = simulation_result.value.logs {
        for log in logs {
            let log_lower = log.to_lowercase();
            if log_lower.contains("fee")
                && (log_lower.contains("transfer") || log_lower.contains("tax"))
            {
                return Ok(CheckResult {
                    passed: false,
                    reason: Some("Transfer tax detected in simulation logs".to_string()),
                    duration_ms: elapsed.as_millis() as u64,
                });
            }
            if log_lower.contains("freeze") || log_lower.contains("blacklist") {
                return Ok(CheckResult {
                    passed: false,
                    reason: Some("Freeze or blacklist detected in simulation logs".to_string()),
                    duration_ms: elapsed.as_millis() as u64,
                });
            }
        }
    }

    tracing::debug!(
        mint = %mint_str,
        pool = %pool_str,
        elapsed_ms = elapsed.as_millis(),
        "Simulate sell check PASSED"
    );

    Ok(CheckResult {
        passed: true,
        reason: None,
        duration_ms: elapsed.as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_mint_pubkey_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(check(
            "not_a_valid_pubkey",
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            1_000_000,
            "https://api.mainnet-beta.solana.com",
        ));
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("Invalid mint pubkey"));
    }

    #[test]
    fn test_invalid_pool_pubkey_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let valid_mint = "So11111111111111111111111111111111111111112";
        let result = rt.block_on(check(
            valid_mint,
            "not_a_valid_pubkey",
            1_000_000,
            "https://api.mainnet-beta.solana.com",
        ));
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("Invalid pool pubkey"));
    }

    #[test]
    fn test_constants() {
        assert_eq!(SIMULATION_TIMEOUT_MS, 30);
    }

    /// Integration test — simulates real sell swap transaction on mainnet RPC.
    #[test]
    #[ignore = "Requires mainnet RPC connection"]
    fn test_simulate_sell_mainnet() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let wsol_mint = "So11111111111111111111111111111111111111112";
        let sol_usdc_pool = "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2";
        let result = rt.block_on(check(
            wsol_mint,
            sol_usdc_pool,
            1_000_000, // 0.001 SOL
            "https://api.mainnet-beta.solana.com",
        ));
        assert!(result.duration_ms > 0);
    }
}
