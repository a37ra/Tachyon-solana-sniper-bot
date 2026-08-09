//! Simulate Sell Before Buy (Risk Gate 3.4).
//!
//! Builds a real sell swap transaction and simulates it via RPC to detect honeypots.
//! When `PreBuyCheckContext` already holds pool bytes, skips the extra get_account RTT.

use super::{CheckResult, PreBuyCheckContext};
use crate::tx_builder::{
    meteora_dlmm, meteora_dynamic, pump_fun, raydium_amm_v4, raydium_clmm, raydium_cpmm,
    SwapDirection, SwapInstructions, SwapParams,
};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_transaction::Transaction;
use std::sync::Arc;
use std::time::Duration;

// Official pump.fun bonding-curve program (mainnet).
// Verified against pump.fun public docs / explorer — do not change without re-verification.
// Must match tx_builder::pump_fun::PUMP_FUN_PROGRAM and signal/dex_listeners/pump_fun.rs.
const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const RAYDIUM_CPMM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
const RAYDIUM_CLMM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
const METEORA_DYNAMIC: &str = "Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB";
// Mainnet DLMM program (v2). Also accept legacy id if seen.
const METEORA_DLMM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
const METEORA_DLMM_LEGACY: &str = "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp";
const METEORA_DAMM_V2: &str = "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG";

pub async fn check(
    mint: &str,
    pool_address: &str,
    size_lamports: u64,
    rpc: Arc<RpcClient>,
    timeout_ms: u64,
) -> CheckResult {
    check_with_context(mint, pool_address, size_lamports, rpc, timeout_ms, None).await
}

/// Simulate sell using pre-fetched pool account when available (saves 1 RTT).
pub async fn check_with_context(
    mint: &str,
    pool_address: &str,
    size_lamports: u64,
    rpc: Arc<RpcClient>,
    timeout_ms: u64,
    context: Option<&PreBuyCheckContext>,
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
    let ctx_owned = context.cloned();

    match tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        simulate_sell_transaction(
            rpc,
            mint_pubkey,
            pool_pubkey,
            size_lamports,
            mint_clone,
            pool_clone,
            ctx_owned,
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
                timeout_ms
            );
            CheckResult {
                passed: false,
                reason: Some(format!("Simulation timeout after {}ms", timeout_ms)),
                duration_ms: timeout_ms,
            }
        }
    }
}

async fn simulate_sell_transaction(
    rpc: Arc<RpcClient>,
    mint_pubkey: Pubkey,
    pool_pubkey: Pubkey,
    size_lamports: u64,
    mint_str: String,
    pool_str: String,
    context: Option<PreBuyCheckContext>,
) -> anyhow::Result<CheckResult> {
    let start = std::time::Instant::now();

    // Prefer pre-fetched pool bytes (0 RTT). Only fetch blockhash (+ pool if missing).
    let (pool_data, dex_program, recent_blockhash) = if let Some(ctx) = context.as_ref() {
        if let (Some(data), Some(owner)) = (&ctx.pool_data, &ctx.pool_owner) {
            let blockhash = rpc.get_latest_blockhash().await?;
            (data.clone(), *owner, blockhash)
        } else {
            let (pool_account_result, blockhash_result) =
                tokio::join!(rpc.get_account(&pool_pubkey), rpc.get_latest_blockhash());
            let pool_account = pool_account_result?;
            (pool_account.data, pool_account.owner, blockhash_result?)
        }
    } else {
        let (pool_account_result, blockhash_result) =
            tokio::join!(rpc.get_account(&pool_pubkey), rpc.get_latest_blockhash());
        let pool_account = pool_account_result?;
        (pool_account.data, pool_account.owner, blockhash_result?)
    };

    let dummy_wallet = Pubkey::new_unique();

    let params = SwapParams {
        mint: mint_pubkey,
        pool_address: pool_pubkey,
        dex_program_id: dex_program,
        wallet: dummy_wallet,
        direction: SwapDirection::Sell,
        amount_in: size_lamports,
        minimum_amount_out: 1,
        slippage_bps: 1000,
    };

    let dex_str = dex_program.to_string();
    let swap_instructions_result: anyhow::Result<SwapInstructions> = match dex_str.as_str() {
        RAYDIUM_AMM_V4 => {
            let state = raydium_amm_v4::RaydiumAmmV4PoolState::from_account_data(&pool_data)?;
            raydium_amm_v4::build_swap(&params, &state)
        }
        RAYDIUM_CPMM => {
            let state = raydium_cpmm::CpmmPoolState::from_account_data(&pool_data)?;
            raydium_cpmm::build_swap(&params, &state)
        }
        RAYDIUM_CLMM => {
            let state = raydium_clmm::ClmmPoolState::from_account_data(&pool_data)?;
            raydium_clmm::build_swap(&params, &state, &pool_pubkey, None)
        }
        METEORA_DYNAMIC => {
            let state =
                meteora_dynamic::DynamicAmmPoolState::from_account_data(&pool_data, &pool_pubkey)?;
            meteora_dynamic::build_swap(&params, &state)
        }
        METEORA_DLMM | METEORA_DLMM_LEGACY => {
            let state = meteora_dlmm::DlmmPoolState::from_account_data(&pool_data, &pool_pubkey)?;
            meteora_dlmm::build_swap(&params, &state)
        }
        METEORA_DAMM_V2 => {
            let state = crate::tx_builder::meteora_damm_v2::DammV2PoolState::from_account_data(
                &pool_data,
                &pool_pubkey,
            )?;
            crate::tx_builder::meteora_damm_v2::build_swap(&params, &state)
        }
        PUMP_FUN_PROGRAM => {
            let curve =
                pump_fun::PumpFunBondingCurve::from_account_data(&pool_data, &pool_pubkey)?;
            pump_fun::build_swap(&params, &curve)
        }
        _ => anyhow::bail!("Unsupported DEX program for simulation: {}", dex_str),
    };

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

    let mut instructions = Vec::new();
    instructions.extend(swap_instructions.setup_instructions);
    instructions.extend(swap_instructions.swap_instructions);
    instructions.extend(swap_instructions.cleanup_instructions);

    let mut transaction = Transaction::new_with_payer(&instructions, Some(&dummy_wallet));
    transaction.message.recent_blockhash = recent_blockhash;

    // Unsigned sim with dummy wallet: missing signer/ATA/funds are expected artifacts.
    // Honeypot signals are custom program errors / freeze / tax logs.
    let simulation_result = rpc.simulate_transaction(&transaction).await?;
    let elapsed = start.elapsed();

    let err_str = simulation_result.value.err.map(|e| format!("{:?}", e));
    let logs = simulation_result.value.logs.unwrap_or_default();

    match classify_simulation_outcome(err_str.as_deref(), &logs) {
        SimClass::Honeypot { reason } => {
            tracing::warn!(
                mint = %mint_str,
                pool = %pool_str,
                reason = %reason,
                "Sell simulation classified as honeypot"
            );
            Ok(CheckResult {
                passed: false,
                reason: Some(reason),
                duration_ms: elapsed.as_millis() as u64,
            })
        }
        SimClass::BenignDummyWallet { detail } => {
            tracing::debug!(
                mint = %mint_str,
                detail = %detail,
                "Simulate sell: dummy-wallet artifact — treating as non-honeypot"
            );
            Ok(CheckResult {
                passed: true,
                reason: None,
                duration_ms: elapsed.as_millis() as u64,
            })
        }
        SimClass::Clean => {
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
    }
}

/// Classification of simulateTransaction outcome for honeypot detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimClass {
    /// Real rug/honeypot signal — reject buy.
    Honeypot { reason: String },
    /// Expected failure from dummy unsigned wallet / empty ATA — do not reject.
    BenignDummyWallet { detail: String },
    /// No error (or only uninteresting noise) — pass.
    Clean,
}

/// Pure classifier (unit-tested). Decides honeypot vs dummy-wallet artifact vs clean.
///
/// Dummy wallet has no SOL/tokens/ATAs and does not sign — so AccountNotFound,
/// InsufficientFunds, MissingSignature, etc. are **not** honeypot evidence.
/// Custom program errors + tax/freeze log markers **are**.
pub fn classify_simulation_outcome(err_str: Option<&str>, logs: &[String]) -> SimClass {
    // Log-based honeypot markers (checked even when err is benign — tax may appear in logs).
    for log in logs {
        let log_lower = log.to_lowercase();
        if log_lower.contains("transfer fee")
            || log_lower.contains("transfer tax")
            || (log_lower.contains("tax") && log_lower.contains("transfer"))
        {
            return SimClass::Honeypot {
                reason: "Transfer tax detected in simulation logs".to_string(),
            };
        }
        if log_lower.contains("account is frozen")
            || log_lower.contains("freeze authority")
            || log_lower.contains("blacklist")
        {
            return SimClass::Honeypot {
                reason: "Freeze or blacklist detected in simulation logs".to_string(),
            };
        }
    }

    let Some(err) = err_str else {
        return SimClass::Clean;
    };

    if is_benign_dummy_wallet_error(err) {
        return SimClass::BenignDummyWallet {
            detail: err.to_string(),
        };
    }

    // Program/custom instruction failures (after benign markers already checked).
    if err.contains("Custom")
        || err.contains("ProgramFailed")
        || err.contains("InstructionError")
    {
        return SimClass::Honeypot {
            reason: format!("Sell simulation failed (honeypot check): {}", err),
        };
    }

    // Unknown error strings: fail-closed (safer than silently buying).
    SimClass::Honeypot {
        reason: format!("Sell simulation failed (unclassified error): {}", err),
    }
}

/// Errors expected when simulating with a random unsigned dummy payer.
fn is_benign_dummy_wallet_error(err: &str) -> bool {
    const MARKERS: &[&str] = &[
        "AccountNotFound",
        "InvalidAccountData",
        "MissingSignature",
        "SignatureVerification",
        "BlockhashNotFound",
        "InsufficientFunds", // dummy has 0 SOL / 0 tokens
        "AccountBorrowed",   // account lock race in sim
        "AlreadyInUse",
        "InvalidAccountOwner", // ATA doesn't exist yet for dummy
        "IllegalOwner",
    ];
    MARKERS.iter().any(|m| err.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_mint_pubkey_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let rpc = Arc::new(RpcClient::new(
            "https://api.mainnet-beta.solana.com".to_string(),
        ));
        let result = rt.block_on(check(
            "not_a_valid_pubkey",
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            1_000_000,
            rpc,
            120,
        ));
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("Invalid mint pubkey"));
    }

    #[test]
    fn test_invalid_pool_pubkey_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let rpc = Arc::new(RpcClient::new(
            "https://api.mainnet-beta.solana.com".to_string(),
        ));
        let valid_mint = "So11111111111111111111111111111111111111112";
        let result = rt.block_on(check(valid_mint, "not_a_valid_pubkey", 1_000_000, rpc, 120));
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("Invalid pool pubkey"));
    }

    #[test]
    fn test_pump_program_id_constant() {
        assert_eq!(
            PUMP_FUN_PROGRAM,
            "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
        );
        assert_eq!(
            PUMP_FUN_PROGRAM,
            crate::tx_builder::pump_fun::PUMP_FUN_PROGRAM.to_string()
        );
    }

    #[test]
    fn classify_missing_signer_is_benign() {
        let c = classify_simulation_outcome(
            Some("TransactionError::SignatureFailure / MissingSignature"),
            &[],
        );
        assert!(matches!(c, SimClass::BenignDummyWallet { .. }), "{c:?}");
    }

    #[test]
    fn classify_insufficient_funds_is_benign() {
        let c = classify_simulation_outcome(
            Some("InstructionError(3, InsufficientFunds)"),
            &[],
        );
        assert!(matches!(c, SimClass::BenignDummyWallet { .. }), "{c:?}");
    }

    #[test]
    fn classify_account_not_found_is_benign() {
        let c = classify_simulation_outcome(Some("AccountNotFound"), &[]);
        assert!(matches!(c, SimClass::BenignDummyWallet { .. }), "{c:?}");
    }

    #[test]
    fn classify_custom_program_error_is_honeypot() {
        let c = classify_simulation_outcome(
            Some("InstructionError(2, Custom(6000))"),
            &[],
        );
        match c {
            SimClass::Honeypot { reason } => {
                assert!(reason.contains("honeypot") || reason.contains("Custom"));
            }
            other => panic!("expected Honeypot, got {other:?}"),
        }
    }

    #[test]
    fn classify_transfer_tax_log_is_honeypot_even_if_err_benign() {
        let logs = vec!["Program log: transfer tax applied 5%".to_string()];
        let c = classify_simulation_outcome(Some("AccountNotFound"), &logs);
        match c {
            SimClass::Honeypot { reason } => assert!(reason.to_lowercase().contains("tax")),
            other => panic!("expected Honeypot from tax log, got {other:?}"),
        }
    }

    #[test]
    fn classify_freeze_log_is_honeypot() {
        let logs = vec!["Error: Account is frozen".to_string()];
        let c = classify_simulation_outcome(None, &logs);
        match c {
            SimClass::Honeypot { reason } => assert!(reason.to_lowercase().contains("freeze")),
            other => panic!("expected Honeypot, got {other:?}"),
        }
    }

    #[test]
    fn classify_clean_sim_passes() {
        assert_eq!(
            classify_simulation_outcome(None, &["Program log: success".into()]),
            SimClass::Clean
        );
    }
}
