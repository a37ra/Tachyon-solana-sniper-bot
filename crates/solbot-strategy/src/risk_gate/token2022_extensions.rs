//! Token-2022 Extensions Check (Risk Gate 3.3).
//!
//! Parses TLV extension data from Token-2022 mint accounts per spl-token-2022 layout:
//! - Bytes 0..82: base Mint
//! - Optional zero-padding up to 165 (BASE_ACCOUNT_LENGTH)
//! - 1 byte AccountType (Mint = 1)
//! - TLV entries: type u16 LE + length u16 LE + value[length]
//!
//! Malformed trailing TLV is ignored after valid entries (fail-open on padding noise).
//! Blocked extensions still reject.

use super::{CheckResult, RiskGateConfig};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use std::sync::Arc;
use std::time::Duration;

const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// Base mint size (spl_token::state::Mint::LEN).
const MINT_LEN: usize = 82;
/// Token account base length — mints with extensions are often padded to this before AccountType.
const BASE_ACCOUNT_LENGTH: usize = 165;
/// spl_token_2022::state::AccountType::Mint
const ACCOUNT_TYPE_MINT: u8 = 1;
/// spl_token_2022::extension::ExtensionType::Uninitialized
const EXT_UNINITIALIZED: u16 = 0;

// Official ExtensionType discriminant values (spl-token-2022).
const EXT_TRANSFER_FEE_CONFIG: u16 = 1;
const EXT_MINT_CLOSE_AUTHORITY: u16 = 3;
const EXT_DEFAULT_ACCOUNT_STATE: u16 = 6;
const EXT_NON_TRANSFERABLE: u16 = 9;
const EXT_INTEREST_BEARING: u16 = 10;
const EXT_PERMANENT_DELEGATE: u16 = 12;
const EXT_TRANSFER_HOOK: u16 = 14;
const EXT_METADATA_POINTER: u16 = 18;
const EXT_TOKEN_METADATA: u16 = 19;
const EXT_GROUP_POINTER: u16 = 20;
const EXT_GROUP_MEMBER_POINTER: u16 = 22;
const EXT_SCALED_UI_AMOUNT: u16 = 25;
const EXT_PAUSABLE: u16 = 26;

pub async fn check(mint: &str, rpc: Arc<RpcClient>, config: &RiskGateConfig) -> CheckResult {
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

    let account_info = match rpc.get_account(&mint_pubkey).await {
        Ok(acc) => acc,
        Err(e) => {
            return CheckResult {
                passed: false,
                reason: Some(format!("RPC error fetching mint account: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    evaluate_token2022(
        mint,
        &account_info.owner.to_string(),
        &account_info.data,
        config,
        start.elapsed(),
    )
}

pub async fn check_with_context(
    mint: &str,
    config: &RiskGateConfig,
    context: &crate::risk_gate::PreBuyCheckContext,
) -> CheckResult {
    let start = std::time::Instant::now();

    if Pubkey::try_from(mint).is_err() {
        return CheckResult {
            passed: false,
            reason: Some(format!("Invalid mint pubkey")),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    let (account_data, owner) = match (&context.mint_data, &context.mint_owner) {
        (Some(data), Some(owner)) => (data.as_slice(), owner.to_string()),
        _ => {
            return CheckResult {
                passed: false,
                reason: Some("Mint account not available in context".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    evaluate_token2022(mint, &owner, account_data, config, start.elapsed())
}

fn evaluate_token2022(
    mint: &str,
    owner: &str,
    account_data: &[u8],
    config: &RiskGateConfig,
    elapsed: Duration,
) -> CheckResult {
    if owner == TOKEN_PROGRAM_ID {
        return CheckResult {
            passed: true,
            reason: None,
            duration_ms: elapsed.as_millis() as u64,
        };
    }

    if owner != TOKEN2022_PROGRAM_ID {
        return CheckResult {
            passed: false,
            reason: Some(format!(
                "Mint account owner is not SPL Token or Token-2022 (owner: {})",
                owner
            )),
            duration_ms: elapsed.as_millis() as u64,
        };
    }

    let blocklist = &config.token2022_blocklist;
    let allowlist = &config.token2022_allowlist;

    if !allowlist.is_empty() {
        if allowlist.iter().any(|a| a == "*") {
            return CheckResult {
                passed: true,
                reason: None,
                duration_ms: elapsed.as_millis() as u64,
            };
        }
        if !allowlist.iter().any(|a| a == mint) {
            return CheckResult {
                passed: false,
                reason: Some("Token-2022 mint not in allowlist".to_string()),
                duration_ms: elapsed.as_millis() as u64,
            };
        }
    }

    let parsed = parse_tlv_extensions(account_data);
    let extensions = match parsed {
        Ok(exts) => exts,
        Err(e) => {
            // Should be rare after resilient parser; still fail-closed on total failure.
            tracing::warn!(
                mint = %mint,
                error = %e,
                data_len = account_data.len(),
                "Token-2022 TLV parse failed entirely"
            );
            return CheckResult {
                passed: false,
                reason: Some(format!("Failed to parse Token-2022 extensions: {}", e)),
                duration_ms: elapsed.as_millis() as u64,
            };
        }
    };

    for extension in &extensions {
        let extension_name = extension_type_to_name(extension.extension_type);
        // Match blocklist entries case-insensitively / with aliases
        if is_blocked(&extension_name, blocklist) {
            tracing::warn!(
                mint = %mint,
                extension = %extension_name,
                ext_type = extension.extension_type,
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

    CheckResult {
        passed: true,
        reason: None,
        duration_ms: elapsed.as_millis() as u64,
    }
}

fn is_blocked(name: &str, blocklist: &[String]) -> bool {
    let n = name.to_lowercase();
    blocklist.iter().any(|b| {
        let bl = b.to_lowercase();
        bl == n
            || (bl == "transferfee" && n == "transferfee")
            || (bl == "transfer_fee" && n == "transferfee")
            || (bl == "permanentdelegate" && n == "permanentdelegate")
            || (bl == "permanent_delegate" && n == "permanentdelegate")
            || (bl == "transferhook" && n == "transferhook")
            || (bl == "transfer_hook" && n == "transferhook")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenExtension {
    extension_type: u16,
    length: usize,
    data: Vec<u8>,
}

/// Locate start of TLV blob after base mint + optional padding + AccountType byte.
///
/// Mirrors spl-token-2022 `type_and_tlv_indices` for Mint.
pub fn tlv_payload_start(data: &[u8]) -> anyhow::Result<Option<usize>> {
    if data.len() < MINT_LEN {
        anyhow::bail!(
            "Token-2022 mint data too short: {} bytes (expected >= {})",
            data.len(),
            MINT_LEN
        );
    }
    if data.len() == MINT_LEN {
        return Ok(None); // no extensions
    }

    // Common case: padded to >= 165, AccountType at index 165, TLV at 166.
    if data.len() > BASE_ACCOUNT_LENGTH {
        let at = data[BASE_ACCOUNT_LENGTH];
        if at == ACCOUNT_TYPE_MINT {
            return Ok(Some(BASE_ACCOUNT_LENGTH + 1));
        }
    }

    // Unpadded: AccountType immediately after mint (index 82).
    let at82 = data[MINT_LEN];
    if at82 == ACCOUNT_TYPE_MINT {
        return Ok(Some(MINT_LEN + 1));
    }

    // Padding zeros between 82 and AccountType.
    if at82 == 0 {
        let mut i = MINT_LEN;
        while i < data.len() && data[i] == 0 {
            i += 1;
        }
        if i < data.len() && data[i] == ACCOUNT_TYPE_MINT {
            return Ok(Some(i + 1));
        }
        // Fixed slot 165 even if intermediate zeros
        if data.len() > BASE_ACCOUNT_LENGTH && data[BASE_ACCOUNT_LENGTH] == ACCOUNT_TYPE_MINT {
            return Ok(Some(BASE_ACCOUNT_LENGTH + 1));
        }
    }

    // Last resort: some accounts omit AccountType and put TLV at 82.
    // Only accept if first type looks like a known ExtensionType (not garbage).
    if data.len() >= MINT_LEN + 4 {
        let maybe_type = u16::from_le_bytes([data[MINT_LEN], data[MINT_LEN + 1]]);
        if is_plausible_extension_type(maybe_type) {
            tracing::debug!(
                maybe_type,
                "TLV start fallback at mint_len (no AccountType byte)"
            );
            return Ok(Some(MINT_LEN));
        }
    }

    // Data after mint but unrecognized — treat as no dangerous extensions.
    tracing::debug!(
        data_len = data.len(),
        byte82 = data.get(MINT_LEN).copied().unwrap_or(0),
        "Token-2022 mint has post-base data but no AccountType/TLV header — treating as no extensions"
    );
    Ok(None)
}

fn is_plausible_extension_type(t: u16) -> bool {
    matches!(
        t,
        EXT_UNINITIALIZED
            | EXT_TRANSFER_FEE_CONFIG
            | 2 // TransferFeeAmount (account-only, but tolerate)
            | EXT_MINT_CLOSE_AUTHORITY
            | 4 | 5 // confidential
            | EXT_DEFAULT_ACCOUNT_STATE
            | 7 | 8
            | EXT_NON_TRANSFERABLE
            | EXT_INTEREST_BEARING
            | 11
            | EXT_PERMANENT_DELEGATE
            | 13
            | EXT_TRANSFER_HOOK
            | 15 | 16 | 17
            | EXT_METADATA_POINTER
            | EXT_TOKEN_METADATA
            | EXT_GROUP_POINTER
            | 21
            | EXT_GROUP_MEMBER_POINTER
            | 23 | 24
            | EXT_SCALED_UI_AMOUNT
            | EXT_PAUSABLE
    ) || (t > 0 && t < 64) // future extension types in reserved range
}

/// Parse TLV extensions. Resilient: stops at malformed trailing data instead of hard-fail.
pub fn parse_tlv_extensions(data: &[u8]) -> anyhow::Result<Vec<TokenExtension>> {
    let start = match tlv_payload_start(data)? {
        None => return Ok(vec![]),
        Some(s) => s,
    };

    if start >= data.len() {
        return Ok(vec![]);
    }

    let tlv = &data[start..];
    let mut extensions = Vec::new();
    let mut offset = 0;

    while offset + 4 <= tlv.len() {
        let extension_type = u16::from_le_bytes([tlv[offset], tlv[offset + 1]]);
        let length = u16::from_le_bytes([tlv[offset + 2], tlv[offset + 3]]) as usize;

        // Uninitialized marks free/padding region — end of used TLV.
        if extension_type == EXT_UNINITIALIZED {
            break;
        }

        // Length exceeds buffer: trailing garbage / wrong offset mid-stream.
        // Keep extensions already parsed; do not fail the whole mint.
        if offset + 4 + length > tlv.len() {
            tracing::debug!(
                extension_type,
                length,
                remaining = tlv.len().saturating_sub(offset + 4),
                parsed = extensions.len(),
                "TLV entry truncated — stopping parse, keeping prior extensions"
            );
            break;
        }

        let value = tlv[offset + 4..offset + 4 + length].to_vec();
        extensions.push(TokenExtension {
            extension_type,
            length,
            data: value,
        });

        offset += 4 + length;
        // spl-token-2022 does not pad between TLV entries (tight packing).
    }

    Ok(extensions)
}

fn extension_type_to_name(extension_type: u16) -> String {
    match extension_type {
        EXT_TRANSFER_FEE_CONFIG => "transferFee".to_string(),
        EXT_MINT_CLOSE_AUTHORITY => "closeAuthority".to_string(),
        EXT_DEFAULT_ACCOUNT_STATE => "defaultAccountState".to_string(),
        EXT_NON_TRANSFERABLE => "nonTransferable".to_string(),
        EXT_INTEREST_BEARING => "interestBearing".to_string(),
        EXT_PERMANENT_DELEGATE => "permanentDelegate".to_string(),
        EXT_TRANSFER_HOOK => "transferHook".to_string(),
        EXT_METADATA_POINTER => "metadataPointer".to_string(),
        EXT_TOKEN_METADATA => "tokenMetadata".to_string(),
        EXT_GROUP_POINTER => "groupPointer".to_string(),
        EXT_GROUP_MEMBER_POINTER => "groupMemberPointer".to_string(),
        EXT_PAUSABLE => "pausable".to_string(),
        EXT_SCALED_UI_AMOUNT => "scaledUiAmount".to_string(),
        _ => format!("unknown_extension_{}", extension_type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mint_base() -> Vec<u8> {
        let mut d = vec![0u8; MINT_LEN];
        d[45] = 1; // initialized
        d
    }

    /// Build mint + padding to 165 + AccountType::Mint + TLV blob.
    fn mint_with_tlv(tlv: &[u8]) -> Vec<u8> {
        let mut d = mint_base();
        d.resize(BASE_ACCOUNT_LENGTH, 0);
        d.push(ACCOUNT_TYPE_MINT);
        d.extend_from_slice(tlv);
        d
    }

    #[test]
    fn extension_type_names_match_spl() {
        assert_eq!(extension_type_to_name(EXT_TRANSFER_FEE_CONFIG), "transferFee");
        assert_eq!(
            extension_type_to_name(EXT_PERMANENT_DELEGATE),
            "permanentDelegate"
        );
        assert_eq!(extension_type_to_name(EXT_TRANSFER_HOOK), "transferHook");
        assert_eq!(
            extension_type_to_name(EXT_METADATA_POINTER),
            "metadataPointer"
        );
        // old wrong ids must NOT map to dangerous names
        assert_eq!(extension_type_to_name(2), "unknown_extension_2");
        assert_eq!(extension_type_to_name(3), "closeAuthority");
    }

    #[test]
    fn tlv_start_none_for_bare_mint() {
        assert!(tlv_payload_start(&mint_base()).unwrap().is_none());
    }

    #[test]
    fn tlv_start_after_padding_and_account_type() {
        let d = mint_with_tlv(&[]);
        assert_eq!(tlv_payload_start(&d).unwrap(), Some(BASE_ACCOUNT_LENGTH + 1));
    }

    #[test]
    fn parse_transfer_fee_with_official_layout() {
        // type=1 length=4 value=01020304
        let tlv = [1u8, 0, 4, 0, 1, 2, 3, 4];
        let data = mint_with_tlv(&tlv);
        let exts = parse_tlv_extensions(&data).unwrap();
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].extension_type, EXT_TRANSFER_FEE_CONFIG);
        assert_eq!(exts[0].data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn parse_multiple_extensions() {
        let mut tlv = vec![1u8, 0, 4, 0, 1, 2, 3, 4]; // transfer fee
        tlv.extend_from_slice(&[12u8, 0, 32, 0]); // permanent delegate type=12
        tlv.extend_from_slice(&[0u8; 32]);
        let data = mint_with_tlv(&tlv);
        let exts = parse_tlv_extensions(&data).unwrap();
        assert_eq!(exts.len(), 2);
        assert_eq!(exts[0].extension_type, EXT_TRANSFER_FEE_CONFIG);
        assert_eq!(exts[1].extension_type, EXT_PERMANENT_DELEGATE);
    }

    #[test]
    fn truncated_tlv_keeps_prior_extensions_no_hard_fail() {
        // valid first ext, then bogus length that exceeds buffer
        let mut tlv = vec![1u8, 0, 4, 0, 1, 2, 3, 4];
        tlv.extend_from_slice(&[14u8, 0, 255, 0]); // transferHook len=255 but no body
        tlv.extend_from_slice(&[0u8; 10]);
        let data = mint_with_tlv(&tlv);
        let exts = parse_tlv_extensions(&data).unwrap();
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].extension_type, EXT_TRANSFER_FEE_CONFIG);
    }

    #[test]
    fn uninitialized_type_ends_tlv() {
        let mut tlv = vec![1u8, 0, 2, 0, 9, 9];
        tlv.extend_from_slice(&[0u8, 0, 0, 0]); // Uninitialized
        tlv.extend_from_slice(&[0xFF; 20]); // garbage after
        let data = mint_with_tlv(&tlv);
        let exts = parse_tlv_extensions(&data).unwrap();
        assert_eq!(exts.len(), 1);
    }

    #[test]
    fn garbage_length_256_without_account_type_does_not_hard_fail() {
        // Old bug: reading TLV at offset 82 on padded mint saw length=256 and Err'd.
        let mut data = mint_base();
        data.resize(BASE_ACCOUNT_LENGTH, 0);
        // no AccountType — zeros only
        data.extend_from_slice(&[0u8; 50]);
        let exts = parse_tlv_extensions(&data).unwrap();
        assert!(exts.is_empty());
    }

    #[test]
    fn blocklist_catches_transfer_fee() {
        let tlv = [1u8, 0, 4, 0, 1, 2, 3, 4];
        let data = mint_with_tlv(&tlv);
        let config = RiskGateConfig {
            token2022_blocklist: vec!["transferFee".into()],
            token2022_allowlist: vec![],
            ..RiskGateConfig::default()
        };
        let r = evaluate_token2022(
            "DummyMint1111111111111111111111111111111",
            TOKEN2022_PROGRAM_ID,
            &data,
            &config,
            Duration::from_millis(1),
        );
        assert!(!r.passed);
        assert!(r.reason.unwrap().contains("transferFee"));
    }

    #[test]
    fn safe_extensions_pass() {
        // metadata pointer only (type 18)
        let tlv = vec![18u8, 0, 2, 0, 0, 0];
        let data = mint_with_tlv(&tlv);
        let config = RiskGateConfig {
            token2022_blocklist: vec![
                "transferFee".into(),
                "permanentDelegate".into(),
                "transferHook".into(),
            ],
            token2022_allowlist: vec![],
            ..RiskGateConfig::default()
        };
        let r = evaluate_token2022(
            "DummyMint1111111111111111111111111111111",
            TOKEN2022_PROGRAM_ID,
            &data,
            &config,
            Duration::from_millis(1),
        );
        assert!(r.passed, "{:?}", r.reason);
    }

    #[test]
    fn test_parse_tlv_extensions_too_short() {
        let data = vec![0u8; MINT_LEN - 1];
        assert!(parse_tlv_extensions(&data).is_err());
    }

    #[test]
    fn legacy_spl_passes() {
        let config = RiskGateConfig::default();
        let r = evaluate_token2022(
            "So11111111111111111111111111111111111111112",
            TOKEN_PROGRAM_ID,
            &mint_base(),
            &config,
            Duration::from_millis(1),
        );
        assert!(r.passed);
    }
}
