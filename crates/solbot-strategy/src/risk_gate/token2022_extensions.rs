//! Token-2022 Extensions Check (Risk Gate 3.3).
//!
//! Parses TLV (Type-Length-Value) extension data from Token-2022 mint accounts
//! and checks for potentially dangerous extensions:
//!
//! - `transfer_fee`: Fees on transfers (can be used for hidden taxes)
//! - `permanent_delegate`: Permanent control over the token (centralization risk)
//! - `transfer_hook`: Custom logic on transfers (can be used for honeypots)
//!
//! Uses allowlist/blocklist matching to determine if extensions are acceptable.

use super::{CheckResult, RiskGateConfig};
use solana_rpc_client::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use std::time::Duration;

/// SPL Token program ID (legacy)
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// Token-2022 program ID
const TOKEN2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// Token-2022 extension type identifiers (from spl-token-2022)
const TRANSFER_FEE_EXTENSION_TYPE: u16 = 1;
const PERMANENT_DELEGATE_EXTENSION_TYPE: u16 = 2;
const TRANSFER_HOOK_EXTENSION_TYPE: u16 = 3;
const METADATA_POINTER_EXTENSION_TYPE: u16 = 4;
const CLOSE_AUTHORITY_EXTENSION_TYPE: u16 = 6;

/// Base mint data size (82 bytes) - same as legacy SPL Token
const MINT_DATA_SIZE: usize = 82;

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
    
    let blocklist = config.token2022_blocklist.clone();
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
            
            // If it's legacy SPL Token, no extensions to check
            if owner == TOKEN_PROGRAM_ID {
                tracing::debug!(mint = %mint, "Legacy SPL Token - no extensions to check");
                return CheckResult {
                    passed: true,
                    reason: None,
                    duration_ms: elapsed.as_millis() as u64,
                };
            }
            
            // If it's not Token-2022, reject
            if owner != TOKEN2022_PROGRAM_ID {
                return CheckResult {
                    passed: false,
                    reason: Some(format!("Mint owner is not SPL Token or Token-2022 (owner: {})", owner)),
                    duration_ms: elapsed.as_millis() as u64,
                };
            }
            
            // Parse TLV extensions from Token-2022 mint data
            let extensions = match parse_tlv_extensions(&account_info.data) {
                Ok(exts) => exts,
                Err(e) => {
                    tracing::error!(
                        mint = %mint,
                        error = %e,
                        data_len = account_info.data.len(),
                        "Failed to parse Token-2022 TLV extensions"
                    );
                    return CheckResult {
                        passed: false,
                        reason: Some(format!("Failed to parse Token-2022 extensions: {}", e)),
                        duration_ms: elapsed.as_millis() as u64,
                    };
                }
            };
            
            tracing::debug!(
                mint = %mint,
                extensions = ?extensions,
                "Token-2022 extensions detected"
            );
            
            // Check if mint is in allowlist
            if !allowlist.is_empty() {
                if allowlist.contains(&"*".to_string()) {
                    // Wildcard allowlist - all Token-2022 tokens accepted
                    tracing::debug!(mint = %mint, "Token-2022 in wildcard allowlist");
                    return CheckResult {
                        passed: true,
                        reason: None,
                        duration_ms: elapsed.as_millis() as u64,
                    };
                }
                
                if !allowlist.contains(&mint.to_string()) {
                    return CheckResult {
                        passed: false,
                        reason: Some(format!("Token-2022 mint not in allowlist")),
                        duration_ms: elapsed.as_millis() as u64,
                    };
                }
            }
            
            // Check for blocked extensions
            for extension in &extensions {
                let extension_name = extension_type_to_name(extension.extension_type);
                
                if blocklist.contains(&extension_name) {
                    tracing::warn!(
                        mint = %mint,
                        extension = %extension_name,
                        "Token-2022 has blocked extension"
                    );
                    return CheckResult {
                        passed: false,
                        reason: Some(format!(
                            "Token-2022 has blocked extension: {}",
                            extension_name
                        )),
                        duration_ms: elapsed.as_millis() as u64,
                    };
                }
            }
            
            // If no blocked extensions found, pass
            tracing::debug!(mint = %mint, "Token-2022 extensions check PASSED");
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

/// Represents a parsed Token-2022 extension.
#[derive(Debug, Clone)]
struct TokenExtension {
    extension_type: u16,
    length: usize,
    data: Vec<u8>,
}

/// Parse TLV (Type-Length-Value) extensions from Token-2022 mint data.
///
/// Token-2022 mint data layout:
/// - Bytes 0-81: Base mint data (same as legacy SPL Token)
/// - Bytes 82+: TLV extensions
fn parse_tlv_extensions(data: &[u8]) -> anyhow::Result<Vec<TokenExtension>> {
    if data.len() < MINT_DATA_SIZE {
        anyhow::bail!("Token-2022 mint data too short: {} bytes (expected >= {})", data.len(), MINT_DATA_SIZE);
    }
    
    let extensions_data = &data[MINT_DATA_SIZE..];
    let mut extensions = Vec::new();
    let mut offset = 0;
    
    while offset + 4 <= extensions_data.len() {
        // TLV format: type (2 bytes) + length (2 bytes) + value (length bytes)
        let extension_type = u16::from_le_bytes([
            extensions_data[offset],
            extensions_data[offset + 1],
        ]);
        
        let length = u16::from_le_bytes([
            extensions_data[offset + 2],
            extensions_data[offset + 3],
        ]) as usize;
        
        if offset + 4 + length > extensions_data.len() {
            anyhow::bail!("Invalid TLV extension: length {} exceeds remaining data", length);
        }
        
        let value = extensions_data[offset + 4..offset + 4 + length].to_vec();
        
        extensions.push(TokenExtension {
            extension_type,
            length,
            data: value,
        });
        
        offset += 4 + length;
    }
    
    Ok(extensions)
}

/// Convert extension type ID to human-readable name.
fn extension_type_to_name(extension_type: u16) -> String {
    match extension_type {
        TRANSFER_FEE_EXTENSION_TYPE => "transferFee".to_string(),
        PERMANENT_DELEGATE_EXTENSION_TYPE => "permanentDelegate".to_string(),
        TRANSFER_HOOK_EXTENSION_TYPE => "transferHook".to_string(),
        METADATA_POINTER_EXTENSION_TYPE => "metadataPointer".to_string(),
        CLOSE_AUTHORITY_EXTENSION_TYPE => "closeAuthority".to_string(),
        _ => format!("unknown_extension_{}", extension_type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_type_to_name() {
        assert_eq!(extension_type_to_name(TRANSFER_FEE_EXTENSION_TYPE), "transferFee");
        assert_eq!(extension_type_to_name(PERMANENT_DELEGATE_EXTENSION_TYPE), "permanentDelegate");
        assert_eq!(extension_type_to_name(TRANSFER_HOOK_EXTENSION_TYPE), "transferHook");
        assert_eq!(extension_type_to_name(METADATA_POINTER_EXTENSION_TYPE), "metadataPointer");
        assert_eq!(extension_type_to_name(CLOSE_AUTHORITY_EXTENSION_TYPE), "closeAuthority");
        assert_eq!(extension_type_to_name(999), "unknown_extension_999");
    }

    #[test]
    fn test_parse_tlv_extensions_empty() {
        let data = vec![0u8; MINT_DATA_SIZE];
        let result = parse_tlv_extensions(&data).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_tlv_extensions_single() {
        let mut data = vec![0u8; MINT_DATA_SIZE];
        // Add a transfer_fee extension: type=1, length=4, value=[1,2,3,4]
        data.extend_from_slice(&[1u8, 0, 4, 0]); // type (little-endian u16) + length (little-endian u16)
        data.extend_from_slice(&[1, 2, 3, 4]); // value
        
        let result = parse_tlv_extensions(&data).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].extension_type, TRANSFER_FEE_EXTENSION_TYPE);
        assert_eq!(result[0].length, 4);
        assert_eq!(result[0].data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_parse_tlv_extensions_multiple() {
        let mut data = vec![0u8; MINT_DATA_SIZE];
        // Add transfer_fee extension
        data.extend_from_slice(&[1u8, 0, 4, 0]);
        data.extend_from_slice(&[1, 2, 3, 4]);
        // Add permanent_delegate extension
        data.extend_from_slice(&[2u8, 0, 32, 0]);
        data.extend_from_slice(&[0u8; 32]);
        
        let result = parse_tlv_extensions(&data).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].extension_type, TRANSFER_FEE_EXTENSION_TYPE);
        assert_eq!(result[1].extension_type, PERMANENT_DELEGATE_EXTENSION_TYPE);
    }

    #[test]
    fn test_parse_tlv_extensions_too_short() {
        let data = vec![0u8; MINT_DATA_SIZE - 1];
        let result = parse_tlv_extensions(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tlv_extensions_invalid_length() {
        let mut data = vec![0u8; MINT_DATA_SIZE];
        // Extension with length that exceeds remaining data
        data.extend_from_slice(&[1u8, 0, 255, 0]); // length = 255
        data.extend_from_slice(&[0u8; 10]); // only 10 bytes of value
        
        let result = parse_tlv_extensions(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_pubkey_rejected() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let config = RiskGateConfig::default();
        let result = rt.block_on(check("not_a_valid_pubkey", "https://api.mainnet-beta.solana.com", &config));
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("Invalid mint pubkey"));
    }

    /// Integration test — checks WSOL (legacy SPL token) on mainnet RPC.
    #[test]
    #[ignore = "Requires mainnet RPC connection"]
    fn test_legacy_token_passes_extension_check() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let config = RiskGateConfig::default();
        let wsol_mint = "So11111111111111111111111111111111111111112";
        let result = rt.block_on(check(wsol_mint, "https://api.mainnet-beta.solana.com", &config));
        assert!(result.passed, "Legacy token should pass extensions check: {:?}", result.reason);
    }
}
