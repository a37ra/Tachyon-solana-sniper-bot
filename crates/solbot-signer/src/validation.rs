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

use solbot_common::types::intent::SignerIntentRequest;
use anyhow::Result;
use chrono::Utc;

#[derive(Debug)]
pub enum RejectionReason {
    StaleIntent,
    InvalidProgramId,
    SizeTooLarge,
    SequenceTooOld,
    RateLimitExceeded,
    HardCapBpsExceeded,
    UnexpectedDestination,
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
    
    // 4. Absolute size cap
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
            hard_cap_bps: 500, // 5%
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
        req.destination_addresses = vec![
            "safe_wallet".to_string(),
            "attacker_wallet".to_string(),
        ];
        assert!(matches!(
            validate_intent(&req, 0, 10_000_000_000, PROGRAMS, WHITELIST, 5000),
            Err(RejectionReason::UnexpectedDestination)
        ));
    }
}
