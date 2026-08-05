//! LP Lock Check (Risk Gate 3.2).
//!
//! Verifies that the LP token mint owner is either:
//! - A burn address (tokens cannot be transferred out)
//! - A timelock program (tokens are locked for a period)
//!
//! This prevents rug pulls where liquidity providers can drain the pool
//! by withdrawing LP tokens and selling them.

use super::CheckResult;
use solana_rpc_client::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use std::time::Duration;
use std::str::FromStr;

/// System program ID (used for burn checks)
const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
/// SPL Token program ID
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// Token-2022 program ID
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// Known timelock programs (allowlist)
const TIMELOCK_PROGRAMS: &[&str] = &[
    // Add known timelock program IDs here
    // Example: "TimelockProgram1111111111111111111111111111111",
];

pub async fn check(pool_address: &str, dex_program: &str, rpc_url: &str) -> CheckResult {
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
    
    let dex_program_clone = dex_program.to_string();
    
    match tokio::task::spawn_blocking(move || {
        // Step 1: Get pool account data
        let pool_account = rpc.get_account(&pool_pubkey)?;
        
        if pool_account.data.is_empty() {
            return Ok::<_, anyhow::Error>((None, None, start.elapsed()));
        }
        
        // Step 2: Extract LP mint address based on DEX type
        let lp_mint = extract_lp_mint(&pool_account.data, &pool_account.owner.to_string(), &dex_program_clone)?;
        
        // Step 3: Get LP mint account info
        let lp_mint_account = rpc.get_account(&lp_mint)?;
        
        if lp_mint_account.data.is_empty() {
            return Ok::<_, anyhow::Error>((Some(lp_mint), None, start.elapsed()));
        }
        
        Ok::<_, anyhow::Error>((Some(lp_mint), Some(lp_mint_account), start.elapsed()))
    })
    .await
    {
        Ok(Ok((Some(lp_mint), Some(lp_mint_account), elapsed))) => {
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
            
            // Check if owner is a burn address or timelock program
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
            
            // LP mint owner is not locked - potential rug risk
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
        Ok(Ok((Some(lp_mint), None, elapsed))) => {
            CheckResult {
                passed: false,
                reason: Some(format!("LP mint account not found: {}", lp_mint)),
                duration_ms: elapsed.as_millis() as u64,
            }
        }
        Ok(Ok((None, _, elapsed))) => {
            CheckResult {
                passed: false,
                reason: Some("Failed to extract LP mint from pool data".to_string()),
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

/// Extract LP mint address from pool account data based on DEX type.
fn extract_lp_mint(pool_data: &[u8], pool_owner: &str, dex_program: &str) -> anyhow::Result<Pubkey> {
    match dex_program {
        // Raydium AMM v4: LP mint is at offset 336 in pool data (after various fields)
        "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" => {
            // Raydium AMM v4 pool layout (752 bytes)
            // LP mint is at offset 336 (based on account layout from instruction parser)
            if pool_data.len() < 336 + 32 {
                anyhow::bail!("Raydium AMM v4 pool data too short for LP mint extraction");
            }
            read_pubkey(pool_data, 336)
        }
        // Raydium CP-MM: LP mint at offset + 32 * 6 (after discriminator)
        "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" => {
            if pool_data.len() < 8 + 32 * 7 {
                anyhow::bail!("Raydium CP-MM pool data too short for LP mint extraction");
            }
            read_pubkey(pool_data, 8 + 32 * 6)
        }
        // Raydium CLMM: LP mint is derived from pool, not stored in pool state
        // CLMM uses a different model - LP tokens are represented differently
        // For now, we'll skip CLMM LP lock check or use a different approach
        "CAMMCzo5YL8w4VFF8KVHr22GGUsp5VTaW7grrKgrWqK" => {
            // CLMM doesn't have a simple LP mint - uses position-based liquidity
            // We'll pass this check for CLMM pools for now
            // TODO: Implement proper CLMM LP lock check if needed
            tracing::warn!("CLMM LP lock check not fully implemented - passing by default");
            Ok(Pubkey::default()) // Return default to indicate skip
        }
        // Meteora Dynamic AMM: Has vault LP mints
        "Eo7WjKq67rjJQSZxS6z3YKapzY3eMj6Xy8X5EQVn5UaB" => {
            // Use a_vault_lp_mint at offset 192
            if pool_data.len() < 8 + 32 * 7 {
                anyhow::bail!("Meteora Dynamic AMM pool data too short for LP mint extraction");
            }
            read_pubkey(pool_data, 8 + 32 * 6)
        }
        // Meteora DLMM: No simple LP mint - uses bin-based liquidity
        "LBUZKhRxPF3XUpBCjp4kVn14nz88kZjgL6Xm1xS8bKp" => {
            // DLMM doesn't have a simple LP mint
            tracing::warn!("DLMM LP lock check not fully implemented - passing by default");
            Ok(Pubkey::default())
        }
        // Pump.fun: Check if it has LP mint
        "6EF8rrecthR5DkZ8w3bq6z3Y3Y3Y3Y3Y3Y3Y3Y3Y3Y" => {
            // Pump.fun bonding curve - may not have LP mint in traditional sense
            // For now, we'll skip
            tracing::warn!("Pump.fun LP lock check not fully implemented - passing by default");
            Ok(Pubkey::default())
        }
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
    // System program is often used as burn address
    if address == SYSTEM_PROGRAM {
        return true;
    }
    
    // Check if it's the zero address (all zeros)
    if let Ok(pubkey) = Pubkey::from_str(address) {
        if pubkey == Pubkey::default() {
            return true;
        }
    }
    
    false
}

/// Check if the given address is a known timelock program.
fn is_timelock_program(address: &str) -> bool {
    TIMELOCK_PROGRAMS.contains(&address)
}
