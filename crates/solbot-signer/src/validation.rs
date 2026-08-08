//! Intent validation pipeline for the signer.
//!
//! Every incoming SignerIntentRequest passes through these checks
//! BEFORE the keypair is used to sign anything:
//!
//! 1. Sequence monotonicity (anti-replay)
//! 2. Staleness (max 500ms clock drift)
//! 3. Program ID allowlist
//! 4. Max size (absolute lamports cap per tier)
//! 5. Hard cap BPS (slippage sanity — reject if amount_or_pct exceeds cap)
//! 6. Destination address whitelist (reject unexpected recipients)
//! 7. Transaction instruction validation (program_id mismatch detection)

use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use solana_transaction::Transaction;
use solbot_common::types::intent::SignerIntentRequest;

#[derive(Debug)]
pub enum RejectionReason {
    StaleIntent,
    InvalidProgramId,
    SizeTooLarge,
    SequenceTooOld,
    RateLimitExceeded,
    HardCapBpsExceeded,
    UnexpectedDestination,
    ProgramIdMismatch,
    SlippageMismatch,
    DestinationMismatch,
    SizeMismatch,
}

impl RejectionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StaleIntent => "StaleIntent",
            Self::InvalidProgramId => "InvalidProgramId",
            Self::SizeTooLarge => "SizeTooLarge",
            Self::SequenceTooOld => "SequenceTooOld",
            Self::RateLimitExceeded => "RateLimitExceeded",
            Self::HardCapBpsExceeded => "HardCapBpsExceeded",
            Self::UnexpectedDestination => "UnexpectedDestination",
            Self::ProgramIdMismatch => "ProgramIdMismatch",
            Self::SlippageMismatch => "SlippageMismatch",
            Self::DestinationMismatch => "DestinationMismatch",
            Self::SizeMismatch => "SizeMismatch",
        }
    }
}

/// Full validation pipeline.
///
/// # Arguments
/// - `req` — the incoming intent
/// - `last_ipc_seq` — last accepted sequence for this context_key (0 if first)
/// - `max_size_absolute_lamports` — absolute cap from tier config
/// - `allowed_programs` — DEX program IDs that the signer will accept
/// - `destination_whitelist` — if non-empty, all `destination_addresses` must be in this set
/// - `hard_cap_bps_limit` — maximum allowed `hard_cap_bps` value (e.g., 5000 = 50%)
pub fn validate_intent(
    req: &SignerIntentRequest,
    last_ipc_seq: u64,
    max_size_absolute_lamports: u64,
    allowed_programs: &[&str],
    destination_whitelist: &[&str],
    hard_cap_bps_limit: u32,
) -> Result<(), RejectionReason> {
    // 1. Anti-replay: sequence must be strictly monotonic
    if req.ipc_seq <= last_ipc_seq {
        return Err(RejectionReason::SequenceTooOld);
    }

    // 2. Staleness: reject if issued_at is too far in the past
    let now = Utc::now().timestamp_millis() as u64;
    if now > req.issued_at_unix_ms && (now - req.issued_at_unix_ms) > 500 {
        return Err(RejectionReason::StaleIntent);
    }

    // 3. Program ID allowlist
    if !allowed_programs.contains(&req.program_id.as_str()) {
        return Err(RejectionReason::InvalidProgramId);
    }

    // 3.5. Validate that declared program_id matches actual transaction instructions
    // This prevents metadata spoofing attacks
    if let Err(e) =
        validate_transaction_program_ids(&req.unsigned_transaction_base64, &req.program_id)
    {
        return Err(e);
    }

    // 3.6. Validate that actual slippage in instruction matches declared hard_cap_bps
    // This prevents claiming low slippage while submitting high slippage swaps
    if let Err(e) = validate_slippage(
        &req.unsigned_transaction_base64,
        &req.program_id,
        req.hard_cap_bps,
    ) {
        return Err(e);
    }

    // 3.7. Validate that declared destinations are present in transaction accounts
    // This prevents claiming tokens go to safe wallet while sending to attacker
    if let Err(e) =
        validate_destinations(&req.unsigned_transaction_base64, &req.destination_addresses)
    {
        return Err(e);
    }

    // 3.8. Validate that actual amount_in matches declared max_size_lamports
    // This prevents claiming small amount while submitting large amount
    if let Err(e) = validate_amount(
        &req.unsigned_transaction_base64,
        &req.program_id,
        req.max_size_lamports,
    ) {
        return Err(e);
    }

    // 4. Absolute size cap (metadata check - now redundant with decoded validation above)
    if req.max_size_lamports > max_size_absolute_lamports {
        return Err(RejectionReason::SizeTooLarge);
    }

    // 5. Hard cap BPS — reject if the intent's slippage tolerance is insane
    //    This prevents a compromised strategy from sending a swap with 99% slippage.
    //    Typical production limit: 5000 bps (50%) for meme tokens.
    if req.hard_cap_bps > hard_cap_bps_limit {
        tracing::warn!(
            context_key = %req.context_key,
            hard_cap_bps = req.hard_cap_bps,
            limit = hard_cap_bps_limit,
            "Intent hard_cap_bps exceeds signer limit"
        );
        return Err(RejectionReason::HardCapBpsExceeded);
    }

    // 6. Destination address whitelist
    //    If the whitelist is non-empty, every destination in the intent
    //    must appear in the whitelist. This catches compromised strategy
    //    processes trying to send funds to attacker wallets.
    if !destination_whitelist.is_empty() && !req.destination_addresses.is_empty() {
        for dest in &req.destination_addresses {
            if !destination_whitelist.contains(&dest.as_str()) {
                tracing::error!(
                    context_key = %req.context_key,
                    unexpected_dest = %dest,
                    "Intent contains destination NOT in signer whitelist — possible compromise"
                );
                return Err(RejectionReason::UnexpectedDestination);
            }
        }
    }

    Ok(())
}

/// Validate that the declared program_id in intent metadata matches
/// the actual program_ids in the decoded transaction instructions.
///
/// This prevents a compromised strategy from lying about the transaction type
/// (e.g., claiming it's a Raydium swap when it's actually a system transfer).
///
/// # Arguments
/// - `unsigned_tx_base64` — base64-encoded unsigned transaction
/// - `declared_program_id` — program_id declared in intent metadata
///
/// # Returns
/// Ok(()) if validation passes, Err(RejectionReason::ProgramIdMismatch) if mismatch
fn validate_transaction_program_ids(
    unsigned_tx_base64: &str,
    declared_program_id: &str,
) -> Result<(), RejectionReason> {
    // Decode base64 transaction
    let tx_bytes = base64::engine::general_purpose::STANDARD
        .decode(unsigned_tx_base64)
        .map_err(|_| RejectionReason::ProgramIdMismatch)?;

    // Deserialize transaction
    let tx: Transaction =
        bincode::deserialize(&tx_bytes).map_err(|_| RejectionReason::ProgramIdMismatch)?;

    // System Program and Compute Budget Program are utility programs
    const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";
    const COMPUTE_BUDGET_PROGRAM_ID: &str = "ComputeBudget111111111111111111111111111111";

    let mut has_non_utility_instruction = false;
    let mut has_declared_program_instruction = false;

    // Check each instruction's program_id
    for instruction in &tx.message.instructions {
        let program_id_index = usize::from(instruction.program_id_index);
        if program_id_index >= tx.message.account_keys.len() {
            return Err(RejectionReason::ProgramIdMismatch);
        }

        let actual_program_id = tx.message.account_keys[program_id_index].to_string();

        // Skip utility programs (System, Compute Budget)
        if actual_program_id == SYSTEM_PROGRAM_ID || actual_program_id == COMPUTE_BUDGET_PROGRAM_ID
        {
            continue;
        }

        has_non_utility_instruction = true;

        // For non-utility programs, they must match the declared program_id
        if actual_program_id == declared_program_id {
            has_declared_program_instruction = true;
        } else {
            tracing::error!(
                declared_program_id = %declared_program_id,
                actual_program_id = %actual_program_id,
                "Transaction instruction program_id does not match declared program_id"
            );
            return Err(RejectionReason::ProgramIdMismatch);
        }
    }

    // If metadata declares a DEX program but transaction has no such instruction → reject
    if declared_program_id != SYSTEM_PROGRAM_ID && !has_declared_program_instruction {
        tracing::error!(
            declared_program_id = %declared_program_id,
            "Transaction contains no instructions with declared program_id"
        );
        return Err(RejectionReason::ProgramIdMismatch);
    }

    // If transaction has only utility programs but metadata declares DEX → reject
    if !has_non_utility_instruction && declared_program_id != SYSTEM_PROGRAM_ID {
        tracing::error!(
            declared_program_id = %declared_program_id,
            "Transaction contains only utility programs but metadata declares DEX program"
        );
        return Err(RejectionReason::ProgramIdMismatch);
    }

    Ok(())
}

/// Validate that the actual slippage in the decoded transaction instruction
/// matches the declared hard_cap_bps from intent metadata.
///
/// This prevents a compromised strategy from claiming low slippage (e.g., 1%)
/// while actually submitting a swap with high slippage (e.g., 50%).
///
/// # Arguments
/// - `unsigned_tx_base64` — base64-encoded unsigned transaction
/// - `declared_program_id` — program_id declared in intent metadata
/// - `declared_hard_cap_bps` — hard_cap_bps declared in intent metadata
///
/// # Returns
/// Ok(()) if validation passes, Err(RejectionReason::SlippageMismatch) if mismatch
fn validate_slippage(
    unsigned_tx_base64: &str,
    declared_program_id: &str,
    declared_hard_cap_bps: u32,
) -> Result<(), RejectionReason> {
    // Decode base64 transaction
    let tx_bytes = base64::engine::general_purpose::STANDARD
        .decode(unsigned_tx_base64)
        .map_err(|_| RejectionReason::SlippageMismatch)?;

    // Deserialize transaction
    let tx: Transaction =
        bincode::deserialize(&tx_bytes).map_err(|_| RejectionReason::SlippageMismatch)?;

    // Raydium AMM v4 program ID
    const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

    // Only validate slippage for known DEX programs
    if declared_program_id != RAYDIUM_AMM_V4 {
        // Skip validation for other programs (can be extended later)
        return Ok(());
    }

    // Find the DEX instruction (skip System and Compute Budget)
    const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";
    const COMPUTE_BUDGET_PROGRAM_ID: &str = "ComputeBudget111111111111111111111111111111";

    for instruction in &tx.message.instructions {
        let program_id_index = usize::from(instruction.program_id_index);
        if program_id_index >= tx.message.account_keys.len() {
            continue;
        }

        let actual_program_id = tx.message.account_keys[program_id_index].to_string();

        // Skip utility programs
        if actual_program_id == SYSTEM_PROGRAM_ID || actual_program_id == COMPUTE_BUDGET_PROGRAM_ID
        {
            continue;
        }

        // Parse Raydium AMM v4 swap instruction data
        // Format: discriminator (1 byte) + amount_in (u64) + minimum_amount_out (u64)
        if actual_program_id == RAYDIUM_AMM_V4 {
            if instruction.data.len() < 17 {
                tracing::error!(
                    "Raydium instruction data too short: {} bytes",
                    instruction.data.len()
                );
                return Err(RejectionReason::SlippageMismatch);
            }

            // Skip discriminator (first byte)
            let amount_in =
                u64::from_le_bytes(instruction.data[1..9].try_into().unwrap_or([0u8; 8]));
            let minimum_amount_out =
                u64::from_le_bytes(instruction.data[9..17].try_into().unwrap_or([0u8; 8]));

            if amount_in == 0 {
                tracing::error!("Invalid amount_in: 0");
                return Err(RejectionReason::SlippageMismatch);
            }

            // Calculate actual slippage in basis points
            // slippage_bps = ((amount_in - minimum_amount_out) / amount_in) * 10000
            let actual_slippage_bps = if minimum_amount_out >= amount_in {
                0 // No slippage or negative (shouldn't happen)
            } else {
                ((amount_in - minimum_amount_out) as u128 * 10000 / amount_in as u128) as u32
            };

            // Compare with declared hard_cap_bps
            if actual_slippage_bps > declared_hard_cap_bps {
                tracing::error!(
                    declared_hard_cap_bps = declared_hard_cap_bps,
                    actual_slippage_bps = actual_slippage_bps,
                    amount_in = amount_in,
                    minimum_amount_out = minimum_amount_out,
                    "Actual slippage exceeds declared hard_cap_bps"
                );
                return Err(RejectionReason::SlippageMismatch);
            }

            tracing::info!(
                actual_slippage_bps = actual_slippage_bps,
                declared_hard_cap_bps = declared_hard_cap_bps,
                "Slippage validation passed"
            );

            return Ok(());
        }
    }

    // No DEX instruction found - skip validation
    Ok(())
}

/// Validate that the actual amount_in in the decoded transaction instruction
/// matches the declared max_size_lamports from intent metadata.
///
/// This prevents a compromised strategy from claiming a small amount
/// while actually submitting a transaction with a large amount.
///
/// # Arguments
/// - `unsigned_tx_base64` — base64-encoded unsigned transaction
/// - `declared_program_id` — program_id declared in intent metadata
/// - `declared_max_size` — max_size_lamports declared in intent metadata
///
/// # Returns
/// Ok(()) if validation passes, Err(RejectionReason::SizeMismatch) if mismatch
fn validate_amount(
    unsigned_tx_base64: &str,
    declared_program_id: &str,
    declared_max_size: u64,
) -> Result<(), RejectionReason> {
    // Decode base64 transaction
    let tx_bytes = base64::engine::general_purpose::STANDARD
        .decode(unsigned_tx_base64)
        .map_err(|_| RejectionReason::SizeMismatch)?;

    // Deserialize transaction
    let tx: Transaction =
        bincode::deserialize(&tx_bytes).map_err(|_| RejectionReason::SizeMismatch)?;

    // Raydium AMM v4 program ID
    const RAYDIUM_AMM_V4: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

    // Only validate amount for known DEX programs
    if declared_program_id != RAYDIUM_AMM_V4 {
        // Skip validation for other programs (can be extended later)
        return Ok(());
    }

    // Find the DEX instruction (skip System and Compute Budget)
    const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";
    const COMPUTE_BUDGET_PROGRAM_ID: &str = "ComputeBudget111111111111111111111111111111";

    for instruction in &tx.message.instructions {
        let program_id_index = usize::from(instruction.program_id_index);
        if program_id_index >= tx.message.account_keys.len() {
            continue;
        }

        let actual_program_id = tx.message.account_keys[program_id_index].to_string();

        // Skip utility programs
        if actual_program_id == SYSTEM_PROGRAM_ID || actual_program_id == COMPUTE_BUDGET_PROGRAM_ID
        {
            continue;
        }

        // Parse Raydium AMM v4 swap instruction data
        // Format: discriminator (1 byte) + amount_in (u64) + minimum_amount_out (u64)
        if actual_program_id == RAYDIUM_AMM_V4 {
            if instruction.data.len() < 17 {
                tracing::error!(
                    "Raydium instruction data too short: {} bytes",
                    instruction.data.len()
                );
                return Err(RejectionReason::SizeMismatch);
            }

            // Skip discriminator (first byte)
            let amount_in =
                u64::from_le_bytes(instruction.data[1..9].try_into().unwrap_or([0u8; 8]));

            // Compare with declared max_size_lamports
            if amount_in > declared_max_size {
                tracing::error!(
                    declared_max_size = declared_max_size,
                    actual_amount_in = amount_in,
                    "Actual amount_in exceeds declared max_size_lamports"
                );
                return Err(RejectionReason::SizeMismatch);
            }

            tracing::info!(
                actual_amount_in = amount_in,
                declared_max_size = declared_max_size,
                "Amount validation passed"
            );

            return Ok(());
        }
    }

    // No DEX instruction found - skip validation
    Ok(())
}

/// Validate that the actual destination account keys in the decoded transaction
/// match the declared destination_addresses from intent metadata.
///
/// This prevents a compromised strategy from claiming tokens go to a safe wallet
/// while actually sending them to an attacker-controlled address.
///
/// # Arguments
/// - `unsigned_tx_base64` — base64-encoded unsigned transaction
/// - `declared_destinations` — destination_addresses declared in intent metadata
///
/// # Returns
/// Ok(()) if validation passes, Err(RejectionReason::DestinationMismatch) if mismatch
fn validate_destinations(
    unsigned_tx_base64: &str,
    declared_destinations: &[String],
) -> Result<(), RejectionReason> {
    // If no destinations declared in metadata, skip validation
    if declared_destinations.is_empty() {
        return Ok(());
    }

    // Decode base64 transaction
    let tx_bytes = base64::engine::general_purpose::STANDARD
        .decode(unsigned_tx_base64)
        .map_err(|_| RejectionReason::DestinationMismatch)?;

    // Deserialize transaction
    let tx: Transaction =
        bincode::deserialize(&tx_bytes).map_err(|_| RejectionReason::DestinationMismatch)?;

    // Collect all account keys from the transaction
    let actual_accounts: std::collections::HashSet<String> = tx
        .message
        .account_keys
        .iter()
        .map(|key| key.to_string())
        .collect();

    // Check that each declared destination is present in the transaction
    for declared_dest in declared_destinations {
        if !actual_accounts.contains(declared_dest) {
            tracing::error!(
                declared_destination = %declared_dest,
                "Declared destination not found in transaction accounts"
            );
            return Err(RejectionReason::DestinationMismatch);
        }
    }

    tracing::info!(
        declared_destinations_count = declared_destinations.len(),
        "Destination validation passed"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use solbot_common::types::intent::IntentType;

    fn make_valid_req(ipc_seq: u64) -> SignerIntentRequest {
        let now = Utc::now().timestamp_millis() as u64;
        SignerIntentRequest {
            context_key: "test_mint".to_string(),
            ipc_seq,
            issued_at_unix_ms: now,
            intent_type: IntentType::Buy,
            unsigned_transaction_base64: "dGVzdA==".to_string(),
            program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            max_size_lamports: 1_000_000_000, // 1 SOL
            hard_cap_bps: 500,                // 5%
            destination_addresses: vec!["safe_wallet".to_string()],
            mint: "test_mint".to_string(),
            pool_address: "test_pool".to_string(),
            position_id: None,
            amount_or_pct: 1_000_000_000.0,
        }
    }

    const PROGRAMS: &[&str] = &["675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8"];
    const WHITELIST: &[&str] = &["safe_wallet"];

    #[test]
    fn test_valid_intent_passes() {
        let req = make_valid_req(1);
        assert!(validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000).is_ok());
    }

    #[test]
    fn test_replay_rejected() {
        let req = make_valid_req(5);
        assert!(matches!(
            validate_intent(&req, 5, 10_000_000_000, PROGRAMS, WHITELIST, 5000),
            Err(RejectionReason::SequenceTooOld)
        ));
    }

    #[test]
    fn test_stale_rejected() {
        let mut req = make_valid_req(1);
        req.issued_at_unix_ms = 1000; // way in the past
        assert!(matches!(
            validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000),
            Err(RejectionReason::StaleIntent)
        ));
    }

    #[test]
    fn test_invalid_program_rejected() {
        let mut req = make_valid_req(1);
        req.program_id = "EVIL_PROGRAM".to_string();
        assert!(matches!(
            validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000),
            Err(RejectionReason::InvalidProgramId)
        ));
    }

    #[test]
    fn test_size_too_large_rejected() {
        let mut req = make_valid_req(1);
        req.max_size_lamports = 100_000_000_000; // 100 SOL
        assert!(matches!(
            validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000),
            Err(RejectionReason::SizeTooLarge)
        ));
    }

    #[test]
    fn test_hard_cap_bps_exceeded() {
        let mut req = make_valid_req(1);
        req.hard_cap_bps = 9900; // 99% — clearly malicious
        assert!(matches!(
            validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000),
            Err(RejectionReason::HardCapBpsExceeded)
        ));
    }

    #[test]
    fn test_hard_cap_bps_at_limit_ok() {
        let mut req = make_valid_req(1);
        req.hard_cap_bps = 5000; // exactly at limit
        assert!(validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000).is_ok());
    }

    #[test]
    fn test_unexpected_destination_rejected() {
        let mut req = make_valid_req(1);
        req.destination_addresses = vec!["attacker_wallet".to_string()];
        assert!(matches!(
            validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000),
            Err(RejectionReason::UnexpectedDestination)
        ));
    }

    #[test]
    fn test_empty_whitelist_allows_any_destination() {
        let mut req = make_valid_req(1);
        req.destination_addresses = vec!["any_wallet".to_string()];
        let empty_whitelist: &[&str] = &[];
        assert!(validate_intent(&req, 0, 10_000_000_000, PROGRAMS, empty_whitelist, 5000).is_ok());
    }

    #[test]
    fn test_multiple_destinations_one_bad() {
        let mut req = make_valid_req(1);
        req.destination_addresses = vec!["safe_wallet".to_string(), "attacker_wallet".to_string()];
        assert!(matches!(
            validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000),
            Err(RejectionReason::UnexpectedDestination)
        ));
    }
}
