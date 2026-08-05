//! Mint authority & freeze authority check (Risk Gate 3.1).
//!
//! Fetches the Mint account via `getAccountInfo`, parses the SPL Token
//! (or Token-2022) Mint struct, and rejects tokens that still have
//! `mint_authority` or `freeze_authority` set.
//!
//! - `mint_authority != None` → the creator can mint unlimited supply (rug risk)
//! - `freeze_authority != None` → the creator can freeze your tokens (honeypot)
//!
//! Both must be `COption::None` for a token to pass this check.

use super::CheckResult;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::program_pack::Pack;
use std::time::Duration;

/// SPL Token program ID
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// Token-2022 (Token Extensions) program ID
const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

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
                tracing::warn!(
                    mint = %mint,
                    elapsed_ms = elapsed.as_millis(),
                    "Mint authority check exceeded 20ms"
                );
            }

            let owner = account_info.owner.to_string();

            // Determine if this is SPL Token or Token-2022
            // Both use the same first 82 bytes for the Mint struct layout,
            // so spl_token::state::Mint::unpack works for both.
            if owner != TOKEN_PROGRAM_ID && owner != TOKEN_2022_PROGRAM_ID {
                return CheckResult {
                    passed: false,
                    reason: Some(format!(
                        "Mint account owner is not SPL Token or Token-2022 (owner: {})",
                        owner
                    )),
                    duration_ms: elapsed.as_millis() as u64,
                };
            }

            let is_token_2022 = owner == TOKEN_2022_PROGRAM_ID;

            // SPL Token Mint layout (82 bytes):
            //   [0..4]   COption<Pubkey> mint_authority tag
            //   [4..36]  mint_authority pubkey (if tag == 1)
            //   [36..44] supply (u64)
            //   [44]     decimals (u8)
            //   [45]     is_initialized (bool)
            //   [46..50] COption<Pubkey> freeze_authority tag
            //   [50..82] freeze_authority pubkey (if tag == 1)
            //
            // Token-2022 uses the same base layout but may have TLV extensions
            // after byte 82. We only need the first 82 bytes for this check.
            let mint_data = spl_token::state::Mint::unpack(&account_info.data);

            match mint_data {
                Ok(mint_state) => {
                    let has_mint_authority = mint_state.mint_authority.is_some();
                    let has_freeze_authority = mint_state.freeze_authority.is_some();

                    if has_mint_authority && has_freeze_authority {
                        tracing::warn!(
                            mint = %mint_pubkey,
                            is_token_2022 = is_token_2022,
                            "Token has BOTH mint_authority and freeze_authority — high rug risk"
                        );
                        CheckResult {
                            passed: false,
                            reason: Some(
                                "Token has both mint_authority and freeze_authority (rug + honeypot risk)".to_string()
                            ),
                            duration_ms: elapsed.as_millis() as u64,
                        }
                    } else if has_mint_authority {
                        tracing::warn!(
                            mint = %mint_pubkey,
                            is_token_2022 = is_token_2022,
                            "Token has mint_authority — unlimited supply rug risk"
                        );
                        CheckResult {
                            passed: false,
                            reason: Some(
                                "Token has mint_authority (creator can mint unlimited supply)".to_string()
                            ),
                            duration_ms: elapsed.as_millis() as u64,
                        }
                    } else if has_freeze_authority {
                        tracing::warn!(
                            mint = %mint_pubkey,
                            is_token_2022 = is_token_2022,
                            "Token has freeze_authority — honeypot risk"
                        );
                        CheckResult {
                            passed: false,
                            reason: Some(
                                "Token has freeze_authority (creator can freeze your tokens)".to_string()
                            ),
                            duration_ms: elapsed.as_millis() as u64,
                        }
                    } else {
                        tracing::debug!(
                            mint = %mint_pubkey,
                            is_token_2022 = is_token_2022,
                            supply = mint_state.supply,
                            decimals = mint_state.decimals,
                            "Mint authority check PASSED: no mint_authority, no freeze_authority"
                        );
                        CheckResult {
                            passed: true,
                            reason: None,
                            duration_ms: elapsed.as_millis() as u64,
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        mint = %mint_pubkey,
                        error = %e,
                        data_len = account_info.data.len(),
                        "Failed to parse Mint account data"
                    );
                    CheckResult {
                        passed: false,
                        reason: Some(format!("Failed to parse mint data: {}", e)),
                        duration_ms: elapsed.as_millis() as u64,
                    }
                }
            }
        }
        Ok(Err(e)) => CheckResult {
            passed: false,
            reason: Some(format!("RPC error fetching mint account: {}", e)),
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(e) => CheckResult {
            passed: false,
            reason: Some(format!("Task join error: {}", e)),
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known USDC mint — has freeze_authority set (Circle controls it).
    /// This should FAIL the check.
    const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    /// Known WSOL mint — no mint_authority, no freeze_authority.
    /// This should PASS the check.
    const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

    #[test]
    fn test_invalid_pubkey_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(check("not_a_valid_pubkey", "https://api.mainnet-beta.solana.com"));
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("Invalid mint pubkey"));
    }

    /// Integration test — requires network. Run with:
    /// `cargo test --package solbot-strategy -- risk_gate::mint_freeze_authority::tests::test_wsol_passes --ignored`
    #[test]
    #[ignore = "Requires mainnet RPC connection"]
    fn test_wsol_passes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(check(WSOL_MINT, "https://api.mainnet-beta.solana.com"));
        assert!(result.passed, "WSOL should pass: {:?}", result.reason);
    }

    /// Integration test — USDC has freeze_authority (Circle). Should fail.
    #[test]
    #[ignore = "Requires mainnet RPC connection"]
    fn test_usdc_fails_freeze_authority() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(check(USDC_MINT, "https://api.mainnet-beta.solana.com"));
        assert!(!result.passed, "USDC should fail due to freeze_authority");
        assert!(result.reason.unwrap().contains("freeze_authority"));
    }
}
