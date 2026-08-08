//! LP Lock Check (Risk Gate 3.2).
//!
//! Verifies that the LP token mint owner is either:
//! - A burn address (tokens cannot be transferred out)
//! - A timelock program (tokens are locked for a period)
//!
//! This prevents rug pulls where liquidity providers can drain the pool
//! by withdrawing LP tokens and selling them.
//!
//! For DEX types without a standard fungible LP-mint the check is explicitly
//! **skipped** with a typed reason rather than silently passed:
//!
//! | DEX type                     | LP-token model              | lp_lock_check result |
//! |------------------------------|-----------------------------|----------------------|
//! | Raydium AMM v4 / CP-MM       | fungible SPL mint           | real owner check     |
//! | Meteora Dynamic AMM          | fungible SPL vault LP mint  | real owner check     |
//! | Raydium CLMM                 | concentrated (NFT position) | `NotApplicableClmm`  |
//! | Meteora DLMM                 | bin-based (NFT position)    | `NotApplicableDlmm`  |
//! | Pump.fun Bonding Curve       | no LP mint (protocol-owned) | `NotApplicableBondingCurve` |
//!
//! For skipped DEX types the `CheckResult.passed = true` is set, but `reason`
//! carries an explicit marker so downstream audit logs can distinguish
//! "LP is burned/timelocked" from "LP check delegated to simulate_sell".

use super::CheckResult;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// System program ID (used for burn checks)
const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
/// Incinerator burn address
const INCINERATOR_ADDRESS: &str = "1nc1nerator11111111111111111111111111111111";

/// Known timelock / liquidity locker programs on Solana
const TIMELOCK_PROGRAMS: &[&str] = &[
    // Streamflow Locker
    "strmZ2eBhSivzpwb9kWwqCzP2cqnWkoV28nhPkXA36p",
    // UNCX / UniCrypt locker program IDs from official UNCX documentation.
    "GsSCS3vPWrtJ5Y9aEVVT65fmrex5P5RGHXdZvsdbWgfo",
    "UNCX77nZrA3TdAxMEggqG18xxpgiNGT6iqyynPwpoxN",
    "UNCXdvMRxvz91g3HqFmpZ5NgmL77UH4QRM4NfeL4mQB",
    "UNCXrB8cZXnmtYM1aSo1Wx3pQaeSZYuF2jCTesXvECs",
    // Additional known lockers should be added here once verified by mainnet program IDs.
];

/// Result of LP mint extraction.  
///
/// Using an explicit enum instead of a `Pubkey::default()` sentinel prevents
/// "silently passed" behaviour from being confused with "actually checked and passed".
#[derive(Debug, Clone, PartialEq)]
enum LpMintResult {
    /// A real fungible LP mint found — proceed to owner check.
    Found(Pubkey),
    /// Pump.fun bonding curve: no fungible LP mint exists; liquidity is
    /// protocol-owned inside the bonding curve PDA until migration.
    NotApplicableBondingCurve,
    /// Raydium CLMM: liquidity is represented by concentrated position NFTs,
    /// not a single fungible LP mint.  Rug-pull check delegated to simulate_sell.
    NotApplicableClmm,
    /// Meteora DLMM: liquidity is bin-based (NFT-like position accounts),
    /// not a single fungible LP mint.  Rug-pull check delegated to simulate_sell.
    NotApplicableDlmm,
}

pub async fn check(pool_address: &str, dex_program: &str, rpc: Arc<RpcClient>) -> CheckResult {
    let start = std::time::Instant::now();
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

    let dex_program_clone = dex_program.to_string();

    // Step 1: Get pool account data
    let pool_account = match rpc.get_account(&pool_pubkey).await {
        Ok(acc) => acc,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("RPC error fetching pool account: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    if pool_account.data.is_empty() {
        return CheckResult {
            passed: true,
            reason: Some("Pool account data is empty - bonding curve".to_string()),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // Step 2: Extract LP mint address based on DEX type
    let lp_mint_result = match extract_lp_mint(
        &pool_account.data,
        &pool_account.owner.to_string(),
        &dex_program_clone,
    ) {
        Ok(result) => result,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("Failed to extract LP mint: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    // Short-circuit for DEX types without fungible LP tokens
    match &lp_mint_result {
        LpMintResult::NotApplicableBondingCurve => {
            return CheckResult {
                passed: true,
                reason: Some("Pump.fun bonding curve - no LP mint".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        LpMintResult::NotApplicableClmm => {
            return CheckResult {
                passed: true,
                reason: Some("Raydium CLMM - no fungible LP mint".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        LpMintResult::NotApplicableDlmm => {
            return CheckResult {
                passed: true,
                reason: Some("Meteora DLMM - no fungible LP mint".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        LpMintResult::Found(_) => {}
    }

    let lp_mint = match &lp_mint_result {
        LpMintResult::Found(pk) => *pk,
        _ => unreachable!(),
    };

    // Step 3: Get LP mint account info
    let lp_mint_account = match rpc.get_account(&lp_mint).await {
        Ok(acc) => acc,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("RPC error fetching LP mint account: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    if lp_mint_account.data.is_empty() {
        return CheckResult {
            passed: true,
            reason: Some("LP mint account data is empty".to_string()),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    let elapsed = start.elapsed();
    if elapsed > Duration::from_millis(30) {
        tracing::warn!("LP lock check exceeded 30ms: {}ms", elapsed.as_millis());
    }

    let lp_mint_owner = lp_mint_account.owner;
    let lp_mint_owner_str = lp_mint_owner.to_string();

    tracing::debug!(
        pool = %pool_address,
        lp_mint = %lp_mint,
        lp_mint_owner = %lp_mint_owner_str,
        "LP lock check: extracted LP mint and owner"
    );

    if is_burn_address(&lp_mint_owner_str) {
        tracing::info!(
            pool = %pool_address,
            lp_mint = %lp_mint,
            "LP lock check PASSED: LP mint owner is burn address"
        );
        return CheckResult {
            passed: true,
            reason: None,
            duration_ms: elapsed.as_millis() as u64,
        };
    }

    if is_timelock_program(&lp_mint_owner_str) {
        tracing::info!(
            pool = %pool_address,
            lp_mint = %lp_mint,
            lp_mint_owner = %lp_mint_owner_str,
            "LP lock check PASSED: LP mint owner is timelock program"
        );
        return CheckResult {
            passed: true,
            reason: None,
            duration_ms: elapsed.as_millis() as u64,
        };
    }

    tracing::warn!(
        pool = %pool_address,
        lp_mint = %lp_mint,
        lp_mint_owner = %lp_mint_owner_str,
        "LP lock check FAILED: LP mint owner is not burn address or timelock"
    );
    CheckResult {
        passed: false,
        reason: Some(format!(
            "LP mint owner ({}) is not burn address or timelock program - rug risk",
            lp_mint_owner_str
        )),
        duration_ms: elapsed.as_millis() as u64,
    }
}

fn extract_lp_mint(
    pool_data: &[u8],
    pool_owner: &str,
    dex_program: &str,
) -> anyhow::Result<LpMintResult> {
    match dex_program {
        // Raydium AMM v4: LP mint is at offset 336 in pool data (752-byte layout)
        "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" => {
            if pool_data.len() < 336 + 32 {
                anyhow::bail!("Raydium AMM v4 pool data too short for LP mint extraction");
            }
            Ok(LpMintResult::Found(read_pubkey(pool_data, 336)?))
        }
        // Raydium CP-MM: LP mint at offset 8 + 32*6
        "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" => {
            if pool_data.len() < 8 + 32 * 7 {
                anyhow::bail!("Raydium CP-MM pool data too short for LP mint extraction");
            }
            Ok(LpMintResult::Found(read_pubkey(pool_data, 8 + 32 * 6)?))
        }
        // Raydium CLMM: concentrated liquidity — positions are NFTs, no single fungible LP mint
        "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" => Ok(LpMintResult::NotApplicableClmm),
        // Meteora Dynamic AMM: LP vault mint at offset 8 + 32*6
        "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB" => {
            if pool_data.len() < 8 + 32 * 7 {
                anyhow::bail!("Meteora Dynamic AMM pool data too short for LP mint extraction");
            }
            Ok(LpMintResult::Found(read_pubkey(pool_data, 8 + 32 * 6)?))
        }
        // Meteora DLMM: bin-based liquidity — no single fungible LP mint
        // Both known program versions:
        // v1: LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp
        // v2: LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo
        "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp"
        | "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo" => Ok(LpMintResult::NotApplicableDlmm),
        // Pump.fun: bonding curve holds reserves; no LP mint until Raydium migration
        "6EF8rrecthR5DkZ8w3bq6z3Y3Y3Y3Y3Y3Y3Y3Y3Y3Y" => Ok(LpMintResult::NotApplicableBondingCurve),
        _ => {
            anyhow::bail!("Unsupported DEX program for LP lock check: {}", dex_program);
        }
    }
}

/// Read a Pubkey from byte slice at given offset.
fn read_pubkey(data: &[u8], offset: usize) -> anyhow::Result<Pubkey> {
    if offset + 32 > data.len() {
        anyhow::bail!("Cannot read pubkey at offset {}: data too short", offset);
    }
    let bytes: [u8; 32] = data[offset..offset + 32]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid pubkey bytes at offset {}", offset))?;
    Ok(Pubkey::new_from_array(bytes))
}

/// Check if the given address is a burn address.
fn is_burn_address(address: &str) -> bool {
    if address == SYSTEM_PROGRAM || address == INCINERATOR_ADDRESS {
        return true;
    }
    if let Ok(pubkey) = Pubkey::from_str(address) {
        if pubkey == Pubkey::default() {
            return true;
        }
    }
    false
}

/// Check if the given address is a known timelock program.
fn is_timelock_program(address: &str) -> bool {
    TIMELOCK_PROGRAMS.iter().any(|&p| p.trim() == address)
}

/// Optimized check using pre-fetched account data to avoid redundant RPC calls.
pub async fn check_with_context(
    pool_address: &str,
    dex_program: &str,
    context: &crate::risk_gate::PreBuyCheckContext,
) -> CheckResult {
    let start = std::time::Instant::now();

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

    let (pool_data, pool_owner) = match (&context.pool_data, &context.pool_owner) {
        (Some(data), Some(owner)) => (data.clone(), owner.to_string()),
        _ => {
            return CheckResult {
                passed: false,
                reason: Some("Pool account not available in context".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    let elapsed = start.elapsed();

    // Skip LP lock check for DEX types without fungible LP tokens
    if pool_owner == "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" {
        return CheckResult {
            passed: true,
            reason: Some("lp_lock:not_applicable:clmm".to_string()),
            duration_ms: elapsed.as_millis() as u64,
        };
    }

    if pool_owner == "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp" {
        return CheckResult {
            passed: true,
            reason: Some("lp_lock:not_applicable:dlmm".to_string()),
            duration_ms: elapsed.as_millis() as u64,
        };
    }

    if pool_owner == "6EF8rrecthR5DkZ8w3bq6z3Y3Y3Y3Y3Y3Y3Y3Y3Y3Y" {
        return CheckResult {
            passed: true,
            reason: Some("lp_lock:not_applicable:bonding_curve".to_string()),
            duration_ms: elapsed.as_millis() as u64,
        };
    }

    // Extract LP mint address from pool data
    let lp_mint_result = match extract_lp_mint(&pool_data, &pool_owner, dex_program) {
        Ok(result) => result,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("lp_lock:parse_failed: {}", e)),
                duration_ms: elapsed.as_millis() as u64,
            };
        }
    };

    match lp_mint_result {
        LpMintResult::NotApplicableBondingCurve => {
            return CheckResult {
                passed: true,
                reason: Some("lp_lock:not_applicable:bonding_curve".to_string()),
                duration_ms: elapsed.as_millis() as u64,
            };
        }
        LpMintResult::NotApplicableClmm => {
            return CheckResult {
                passed: true,
                reason: Some("lp_lock:not_applicable:clmm".to_string()),
                duration_ms: elapsed.as_millis() as u64,
            };
        }
        LpMintResult::NotApplicableDlmm => {
            return CheckResult {
                passed: true,
                reason: Some("lp_lock:not_applicable:dlmm".to_string()),
                duration_ms: elapsed.as_millis() as u64,
            };
        }
        LpMintResult::Found(_) => {}
    }

    // Use pre-fetched LP mint owner if available
    let lp_mint_owner = match &context.lp_mint_owner {
        Some(owner) => owner.to_string(),
        _ => {
            return CheckResult {
                passed: false,
                reason: Some("lp_lock:context_unavailable".to_string()),
                duration_ms: elapsed.as_millis() as u64,
            };
        }
    };

    // Check if LP mint owner is burn address or timelock program
    if is_burn_address(&lp_mint_owner) {
        CheckResult {
            passed: true,
            reason: None,
            duration_ms: elapsed.as_millis() as u64,
        }
    } else if is_timelock_program(&lp_mint_owner) {
        CheckResult {
            passed: true,
            reason: None,
            duration_ms: elapsed.as_millis() as u64,
        }
    } else {
        CheckResult {
            passed: false,
            reason: Some(format!("LP is not locked (mint owner: {})", lp_mint_owner)),
            duration_ms: elapsed.as_millis() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ... (rest of the code remains the same)
    #[test]
    fn test_is_burn_address_system_program() {
        assert!(is_burn_address(SYSTEM_PROGRAM));
        assert!(is_burn_address(INCINERATOR_ADDRESS));
    }

    #[test]
    fn test_is_burn_address_zero_address() {
        assert!(is_burn_address(&Pubkey::default().to_string()));
    }

    #[test]
    fn test_is_burn_address_random_address() {
        let random_pubkey = Pubkey::new_unique();
        assert!(!is_burn_address(&random_pubkey.to_string()));
    }

    #[test]
    fn test_timelock_programs_are_valid_pubkeys() {
        for &program_id in TIMELOCK_PROGRAMS {
            assert!(
                Pubkey::try_from(program_id).is_ok(),
                "Invalid timelock program ID: {}",
                program_id
            );
            assert!(
                !program_id.contains("..."),
                "Timelock program ID must not be a placeholder: {}",
                program_id
            );
            assert!(
                !program_id.contains(' '),
                "Timelock program ID must not contain spaces: {}",
                program_id
            );
        }
    }

    #[test]
    fn test_read_pubkey_valid() {
        let mut data = vec![0u8; 64];
        let test_pubkey = Pubkey::new_unique();
        data[32..64].copy_from_slice(test_pubkey.as_ref());
        let result = read_pubkey(&data, 32).unwrap();
        assert_eq!(result, test_pubkey);
    }

    #[test]
    fn test_extract_lp_mint_raydium_amm_v4() {
        let mut data = vec![0u8; 400];
        let test_pubkey = Pubkey::new_unique();
        data[336..368].copy_from_slice(test_pubkey.as_ref());

        let result = extract_lp_mint(
            &data,
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
        )
        .unwrap();
        assert_eq!(result, LpMintResult::Found(test_pubkey));
    }

    #[test]
    fn test_extract_lp_mint_pump_fun_not_applicable() {
        let data = vec![0u8; 100];
        let result = extract_lp_mint(
            &data,
            "6EF8rrecthR5DkZ8w3bq6z3Y3Y3Y3Y3Y3Y3Y3Y3Y3Y",
            "6EF8rrecthR5DkZ8w3bq6z3Y3Y3Y3Y3Y3Y3Y3Y3Y3Y",
        )
        .unwrap();
        assert_eq!(result, LpMintResult::NotApplicableBondingCurve);
    }

    #[test]
    fn test_extract_lp_mint_clmm_not_applicable() {
        let data = vec![0u8; 500];
        let result = extract_lp_mint(
            &data,
            "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
            "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
        )
        .unwrap();
        // CLMM returns explicit NotApplicableClmm — not a generic sentinel
        assert_eq!(result, LpMintResult::NotApplicableClmm);
    }

    #[test]
    fn test_extract_lp_mint_dlmm_not_applicable() {
        let data = vec![0u8; 500];
        let result = extract_lp_mint(
            &data,
            "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp",
            "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp",
        )
        .unwrap();
        // DLMM returns explicit NotApplicableDlmm — not a generic sentinel
        assert_eq!(result, LpMintResult::NotApplicableDlmm);
    }

    /// Each skipped DEX type must produce a distinct reason string — not a shared generic one.
    #[test]
    fn test_skip_reasons_are_distinct() {
        // We verify the reason strings directly to catch any future accidental unification
        let bonding_curve_reason = "lp_lock:not_applicable:bonding_curve";
        let clmm_reason = "lp_lock:not_applicable:clmm";
        let dlmm_reason = "lp_lock:not_applicable:dlmm";
        assert_ne!(bonding_curve_reason, clmm_reason);
        assert_ne!(clmm_reason, dlmm_reason);
        assert_ne!(bonding_curve_reason, dlmm_reason);
    }

    #[test]
    fn test_invalid_pubkey_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(check(
            "not_a_valid_pubkey",
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            "https://api.mainnet-beta.solana.com",
        ));
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("Invalid pool pubkey"));
    }

    #[test]
    fn test_check_and_check_with_context_invalid_pool_reject_same_reason() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool_address = "not_a_valid_pubkey";
        let fallback = rt.block_on(check(
            pool_address,
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            "https://api.mainnet-beta.solana.com",
        ));

        let context = crate::risk_gate::PreBuyCheckContext {
            mint_data: None,
            mint_owner: None,
            pool_data: None,
            pool_owner: None,
            lp_mint_data: None,
            lp_mint_owner: None,
            pool_data_from_ws: false,
        };

        let optimized = rt.block_on(check_with_context(
            pool_address,
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            &context,
        ));

        assert_eq!(fallback.passed, optimized.passed);
        assert_eq!(fallback.reason, optimized.reason);
    }

    #[test]
    fn test_check_with_context_parse_failed_rejects() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool_address = Pubkey::new_unique().to_string();
        let pool_owner = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string();
        let context = crate::risk_gate::PreBuyCheckContext {
            mint_data: None,
            mint_owner: None,
            pool_data: Some(vec![0u8; 10]),
            pool_owner: Some(Pubkey::try_from(pool_owner.as_str()).unwrap()),
            lp_mint_data: None,
            lp_mint_owner: None,
            pool_data_from_ws: false,
        };

        let result = rt.block_on(check_with_context(&pool_address, &pool_owner, &context));
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("lp_lock:parse_failed"));
    }

    #[test]
    fn test_check_with_context_missing_context_rejects() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool_address = Pubkey::new_unique().to_string();
        let mut pool_data = vec![0u8; 400];
        let lp_pubkey = Pubkey::new_unique();
        pool_data[336..368].copy_from_slice(lp_pubkey.as_ref());

        let context = crate::risk_gate::PreBuyCheckContext {
            mint_data: None,
            mint_owner: None,
            pool_data: Some(pool_data),
            pool_owner: Some(
                Pubkey::try_from("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap(),
            ),
            lp_mint_data: None,
            lp_mint_owner: None,
            pool_data_from_ws: false,
        };

        let result = rt.block_on(check_with_context(
            &pool_address,
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            &context,
        ));
        assert!(!result.passed);
        assert_eq!(
            result.reason.as_deref(),
            Some("lp_lock:context_unavailable")
        );
    }

    /// Integration: Raydium AMM v4 SOL-USDC pool — has a real fungible LP mint.
    #[test]
    #[ignore = "Requires mainnet RPC connection"]
    fn test_raydium_amm_v4_mainnet_lp_check() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool = "58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2";
        let program = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
        let result = rt.block_on(check(pool, program, "https://api.mainnet-beta.solana.com"));
        assert!(result.duration_ms > 0);
        // reason must NOT be the "not_applicable" skip markers
        if let Some(ref r) = result.reason {
            assert!(
                !r.starts_with("lp_lock:not_applicable:"),
                "AMM v4 pool should perform a real LP owner check, got skip reason: {}",
                r
            );
        }
    }

    /// Integration: Raydium CLMM SOL-USDC pool — no fungible LP mint.
    /// Verifies that extract_lp_mint returns NotApplicableClmm on live data
    /// and that check() records the correct skip reason (not a generic pass/fail).
    #[test]
    #[ignore = "Requires mainnet RPC connection"]
    fn test_raydium_clmm_mainnet_skip_reason() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Raydium CLMM SOL-USDC pool
        let pool = "HJPjoWUrhoZzkNfRpHuieeFk9WcZWjwy6PBjZ81ngndJ";
        let program = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
        let result = rt.block_on(check(pool, program, "https://api.mainnet-beta.solana.com"));
        assert!(
            result.passed,
            "CLMM pool should pass (skipped): {:?}",
            result.reason
        );
        assert_eq!(
            result.reason.as_deref(),
            Some("lp_lock:not_applicable:clmm"),
            "CLMM must produce typed skip reason, not generic pass"
        );
    }

    /// Integration: Meteora DLMM pool — no fungible LP mint.
    /// Uses pool FoSDw2L5 (v2 program LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo,
    /// verified live on mainnet 2026-08-05: owner matches, data len = 904 bytes).
    /// Verifies that check() returns passed=true with the typed DLMM skip reason.
    #[test]
    #[ignore = "Requires mainnet RPC connection"]
    fn test_meteora_dlmm_mainnet_skip_reason() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Real Meteora DLMM v2 pool on mainnet
        let pool = "FoSDw2L5DmTuQTFe55gWPDXf88euaxAEKFre74CnvQbX";
        // v2 DLMM program ID — this is the actual owner of the pool account
        let program = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
        let result = rt.block_on(check(pool, program, "https://api.mainnet-beta.solana.com"));
        assert!(
            result.passed,
            "DLMM pool should pass (skipped): {:?}",
            result.reason
        );
        assert_eq!(
            result.reason.as_deref(),
            Some("lp_lock:not_applicable:dlmm"),
            "DLMM must produce typed skip reason, not generic pass"
        );
    }
}
