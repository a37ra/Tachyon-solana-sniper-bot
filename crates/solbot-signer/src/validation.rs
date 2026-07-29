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
}

impl RejectionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StaleIntent => "StaleIntent",
            Self::InvalidProgramId => "InvalidProgramId",
            Self::SizeTooLarge => "SizeTooLarge",
            Self::SequenceTooOld => "SequenceTooOld",
            Self::RateLimitExceeded => "RateLimitExceeded",
        }
    }
}

pub fn validate_intent(
    req: &SignerIntentRequest,
    last_ipc_seq: u64,
    max_size_absolute_lamports: u64,
    allowed_programs: &[&str],
) -> Result<(), RejectionReason> {
    if req.ipc_seq <= last_ipc_seq {
        return Err(RejectionReason::SequenceTooOld);
    }
    
    let now = Utc::now().timestamp_millis() as u64;
    if now > req.issued_at_unix_ms && (now - req.issued_at_unix_ms) > 500 {
        return Err(RejectionReason::StaleIntent);
    }
    
    if !allowed_programs.contains(&req.program_id.as_str()) {
        return Err(RejectionReason::InvalidProgramId);
    }
    
    if req.max_size_lamports > max_size_absolute_lamports {
        return Err(RejectionReason::SizeTooLarge);
    }
    
    Ok(())
}
