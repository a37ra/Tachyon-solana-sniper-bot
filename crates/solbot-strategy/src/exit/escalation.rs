use anyhow::Result;
use serde::{Deserialize, Serialize};
use solbot_common::config::escalation_config::EscalationConfig;
use std::time::Instant;

/// 4-stage forward-only escalation state machine (from ТЗ 2.3).
///
/// This is the **unified** escalation path used by:
/// - Hard stop loss (4.4)
/// - Backfill incomplete (2.4)
/// - Circuit breaker on_trigger (4.8)
///
/// Key invariants:
/// - **Forward-only**: stages can only advance, never go back
/// - **Signature reuse**: retries within a stage reuse ONE signature
/// - **New signature**: only created on stage transitions
/// - **Stage 3 is terminal**: infinite retry with durable nonce until manual intervention
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EscalationStage {
    /// Stage 0: Jito Bundle submission.
    /// Tip + randomized jitter, 2 attempts, 500ms timeout each.
    Stage0Jito,

    /// Stage 1: Public RPC with 2× priority fee.
    /// 2 attempts, 500ms timeout each.
    Stage1PublicRpc2x,

    /// Stage 2: Public RPC with 3× priority fee + max slippage.
    /// 3 attempts, 500ms timeout each, backoff [200, 400, 800]ms.
    Stage2PublicRpc3xMaxSlippage,

    /// Stage 3: Durable nonce transaction.
    /// Infinite retry every 5s until success or manual override.
    /// Position status → StuckManualInterventionRequired.
    Stage3DurableNonceStuck,
}

impl EscalationStage {
    /// Advance to the next stage. Returns `None` if already at Stage 3 (terminal).
    pub fn next(&self) -> Option<EscalationStage> {
        match self {
            EscalationStage::Stage0Jito => Some(EscalationStage::Stage1PublicRpc2x),
            EscalationStage::Stage1PublicRpc2x => Some(EscalationStage::Stage2PublicRpc3xMaxSlippage),
            EscalationStage::Stage2PublicRpc3xMaxSlippage => Some(EscalationStage::Stage3DurableNonceStuck),
            EscalationStage::Stage3DurableNonceStuck => None, // terminal
        }
    }

    /// Maximum number of attempts within this stage before transitioning.
    pub fn max_attempts(&self) -> u32 {
        match self {
            EscalationStage::Stage0Jito => 2,
            EscalationStage::Stage1PublicRpc2x => 2,
            EscalationStage::Stage2PublicRpc3xMaxSlippage => 3,
            EscalationStage::Stage3DurableNonceStuck => u32::MAX, // infinite
        }
    }

    /// Timeout per attempt in milliseconds.
    pub fn timeout_ms(&self) -> u64 {
        match self {
            EscalationStage::Stage0Jito => 500,
            EscalationStage::Stage1PublicRpc2x => 500,
            EscalationStage::Stage2PublicRpc3xMaxSlippage => 500,
            EscalationStage::Stage3DurableNonceStuck => 5000,
        }
    }

    /// Priority fee multiplier for this stage.
    pub fn priority_fee_multiplier(&self) -> f64 {
        match self {
            EscalationStage::Stage0Jito => 1.0,
            EscalationStage::Stage1PublicRpc2x => 2.0,
            EscalationStage::Stage2PublicRpc3xMaxSlippage => 3.0,
            EscalationStage::Stage3DurableNonceStuck => 5.0,
        }
    }

    /// Slippage BPS for this stage.
    pub fn slippage_bps(&self) -> u32 {
        match self {
            EscalationStage::Stage0Jito => 500,
            EscalationStage::Stage1PublicRpc2x => 500,
            EscalationStage::Stage2PublicRpc3xMaxSlippage => 1000,
            EscalationStage::Stage3DurableNonceStuck => 1000,
        }
    }

    /// Backoff delays between retries within this stage (milliseconds).
    pub fn backoff_ms(&self) -> &'static [u64] {
        match self {
            EscalationStage::Stage0Jito => &[100],
            EscalationStage::Stage1PublicRpc2x => &[200],
            EscalationStage::Stage2PublicRpc3xMaxSlippage => &[200, 400, 800],
            EscalationStage::Stage3DurableNonceStuck => &[5000],
        }
    }

    /// Whether this stage uses the Jito route.
    pub fn is_jito(&self) -> bool {
        matches!(self, EscalationStage::Stage0Jito)
    }

    /// Whether this stage uses the durable nonce route.
    pub fn is_durable_nonce(&self) -> bool {
        matches!(self, EscalationStage::Stage3DurableNonceStuck)
    }

    /// Whether this stage uses public RPC direct submission.
    pub fn is_public_rpc(&self) -> bool {
        matches!(
            self,
            EscalationStage::Stage1PublicRpc2x | EscalationStage::Stage2PublicRpc3xMaxSlippage
        )
    }
}

impl std::fmt::Display for EscalationStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EscalationStage::Stage0Jito => write!(f, "Stage0Jito"),
            EscalationStage::Stage1PublicRpc2x => write!(f, "Stage1PublicRpc2x"),
            EscalationStage::Stage2PublicRpc3xMaxSlippage => write!(f, "Stage2PublicRpc3xMaxSlippage"),
            EscalationStage::Stage3DurableNonceStuck => write!(f, "Stage3DurableNonceStuck"),
        }
    }
}

/// Trigger source that initiated the escalation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EscalationTrigger {
    HardStopLoss,
    BackfillIncomplete,
    CircuitBreakerOnTrigger,
}

impl std::fmt::Display for EscalationTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EscalationTrigger::HardStopLoss => write!(f, "hard_stop_loss"),
            EscalationTrigger::BackfillIncomplete => write!(f, "backfill_incomplete"),
            EscalationTrigger::CircuitBreakerOnTrigger => write!(f, "circuit_breaker"),
        }
    }
}

/// Persistent state of an escalation sequence for a single position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationState {
    /// Unique ID for this escalation sequence.
    pub escalation_id: String,
    /// The position being emergency-exited.
    pub position_id: String,
    /// Mint address of the token.
    pub mint: String,
    /// Current escalation stage.
    pub current_stage: EscalationStage,
    /// Number of attempts in the current stage.
    pub attempts_in_stage: u32,
    /// What triggered this escalation.
    pub trigger: EscalationTrigger,
    /// The signed transaction signature for the current stage (reused across retries).
    pub current_stage_signature: Option<String>,
    /// All tx signatures across all stages (for audit trail).
    pub all_signatures: Vec<String>,
    /// When the escalation was started (Unix ms).
    pub started_at_unix_ms: u64,
    /// When the current stage was entered (Unix ms).
    pub stage_entered_at_unix_ms: u64,
    /// Whether this escalation has completed (successfully or via manual override).
    pub completed: bool,
    /// Final outcome.
    pub outcome: Option<EscalationOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EscalationOutcome {
    /// Transaction confirmed on-chain.
    Success { signature: String, slot: u64 },
    /// Manual override via CLI.
    ManualOverride { command: String },
    /// Kill switch activated.
    KillSwitch,
}

impl EscalationState {
    /// Create a new escalation starting at Stage 0.
    pub fn new(
        escalation_id: String,
        position_id: String,
        mint: String,
        trigger: EscalationTrigger,
        now_unix_ms: u64,
    ) -> Self {
        Self {
            escalation_id,
            position_id,
            mint,
            current_stage: EscalationStage::Stage0Jito,
            attempts_in_stage: 0,
            trigger,
            current_stage_signature: None,
            all_signatures: Vec::new(),
            started_at_unix_ms: now_unix_ms,
            stage_entered_at_unix_ms: now_unix_ms,
            completed: false,
            outcome: None,
        }
    }

    /// Record an attempt in the current stage.
    ///
    /// Returns `true` if we should retry within this stage,
    /// `false` if all attempts are exhausted and we should transition.
    pub fn record_attempt(&mut self) -> bool {
        self.attempts_in_stage += 1;
        self.attempts_in_stage < self.current_stage.max_attempts()
    }

    /// Advance to the next stage.
    ///
    /// This is a forward-only transition: Stage0 → Stage1 → Stage2 → Stage3.
    /// Returns `true` if successfully advanced, `false` if already at terminal stage.
    ///
    /// **Key**: A new signature MUST be requested from the signer upon transition.
    pub fn advance_stage(&mut self, now_unix_ms: u64) -> bool {
        if let Some(next) = self.current_stage.next() {
            let old_stage = self.current_stage;
            self.current_stage = next;
            self.attempts_in_stage = 0;
            self.current_stage_signature = None; // new signature needed
            self.stage_entered_at_unix_ms = now_unix_ms;

            tracing::warn!(
                escalation_id = %self.escalation_id,
                position_id = %self.position_id,
                from_stage = %old_stage,
                to_stage = %self.current_stage,
                "Escalation advanced to next stage"
            );
            true
        } else {
            tracing::error!(
                escalation_id = %self.escalation_id,
                position_id = %self.position_id,
                "Already at terminal Stage3 — cannot advance further"
            );
            false
        }
    }

    /// Set the signature for the current stage (reused across retries within this stage).
    pub fn set_stage_signature(&mut self, signature: String) {
        self.all_signatures.push(signature.clone());
        self.current_stage_signature = Some(signature);
    }

    /// Mark the escalation as successfully completed.
    pub fn complete_success(&mut self, signature: String, slot: u64) {
        self.completed = true;
        self.outcome = Some(EscalationOutcome::Success { signature, slot });

        tracing::info!(
            escalation_id = %self.escalation_id,
            position_id = %self.position_id,
            stage = %self.current_stage,
            "Escalation completed successfully"
        );
    }

    /// Mark the escalation as completed via manual override.
    pub fn complete_manual_override(&mut self, command: String) {
        self.completed = true;
        self.outcome = Some(EscalationOutcome::ManualOverride { command });

        tracing::info!(
            escalation_id = %self.escalation_id,
            position_id = %self.position_id,
            "Escalation completed via manual override"
        );
    }

    /// Whether a new signature is needed (first attempt in a new stage).
    pub fn needs_new_signature(&self) -> bool {
        self.current_stage_signature.is_none()
    }

    /// Whether this escalation is in the terminal stuck state.
    pub fn is_stuck(&self) -> bool {
        self.current_stage == EscalationStage::Stage3DurableNonceStuck
    }

    /// Get the backoff delay for the current retry attempt (within the stage).
    pub fn current_backoff_ms(&self) -> u64 {
        let backoffs = self.current_stage.backoff_ms();
        let idx = (self.attempts_in_stage as usize).saturating_sub(1);
        *backoffs.get(idx).unwrap_or(backoffs.last().unwrap_or(&100))
    }
}

/// Run the full escalation loop for a position.
///
/// This is the main entry point for emergency exits. It drives the state machine
/// through all stages until the transaction confirms or reaches Stage 3 (stuck).
///
/// # Arguments
/// - `state`: Mutable escalation state (persisted to SQLite after each step)
/// - `sign_fn`: Callback to request a new signature from the signer
/// - `submit_fn`: Callback to submit a signed transaction via the appropriate route
/// - `persist_fn`: Callback to persist state to SQLite
///
/// # Returns
/// - `Ok(true)` if the transaction was confirmed
/// - `Ok(false)` if the escalation reached Stage 3 and is stuck
/// - `Err(e)` on unrecoverable errors
pub async fn run_escalation<SignFn, SubmitFn, PersistFn>(
    state: &mut EscalationState,
    sign_fn: &SignFn,
    submit_fn: &SubmitFn,
    persist_fn: &PersistFn,
) -> Result<bool>
where
    SignFn: Fn(&EscalationState) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>>,
    SubmitFn: Fn(&EscalationState) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<(String, u64)>>> + Send + '_>>,
    PersistFn: Fn(&EscalationState) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>,
{
    loop {
        if state.completed {
            return Ok(true);
        }

        // 1. Get a signature if needed (new stage or first attempt)
        if state.needs_new_signature() {
            let signature = sign_fn(state).await?;
            state.set_stage_signature(signature);
            persist_fn(state).await?;
        }

        // 2. Submit the transaction
        let submit_result = submit_fn(state).await;

        match submit_result {
            Ok(Some((sig, slot))) => {
                // Transaction confirmed!
                state.complete_success(sig, slot);
                persist_fn(state).await?;
                return Ok(true);
            }
            Ok(None) => {
                // Submission failed / no confirmation
                tracing::warn!(
                    escalation_id = %state.escalation_id,
                    stage = %state.current_stage,
                    attempt = state.attempts_in_stage,
                    "Escalation attempt failed"
                );
            }
            Err(e) => {
                tracing::error!(
                    escalation_id = %state.escalation_id,
                    stage = %state.current_stage,
                    error = %e,
                    "Escalation submission error"
                );
            }
        }

        // 3. Record the attempt
        let should_retry = state.record_attempt();
        persist_fn(state).await?;

        if should_retry {
            // Retry within same stage with backoff
            let backoff = state.current_backoff_ms();
            tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        } else {
            // All attempts exhausted — advance to next stage
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            if !state.advance_stage(now_ms) {
                // Already at Stage 3 (terminal) — stuck
                persist_fn(state).await?;

                // In Stage 3, we continue retrying with durable nonce every 5s
                // but caller should handle this as a background task
                return Ok(false);
            }
            persist_fn(state).await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> EscalationState {
        EscalationState::new(
            "esc-1".to_string(),
            "pos-1".to_string(),
            "Mint1".to_string(),
            EscalationTrigger::HardStopLoss,
            1000000,
        )
    }

    #[test]
    fn test_stage_progression() {
        let mut state = make_state();
        assert_eq!(state.current_stage, EscalationStage::Stage0Jito);

        assert!(state.advance_stage(1000100));
        assert_eq!(state.current_stage, EscalationStage::Stage1PublicRpc2x);

        assert!(state.advance_stage(1000200));
        assert_eq!(state.current_stage, EscalationStage::Stage2PublicRpc3xMaxSlippage);

        assert!(state.advance_stage(1000300));
        assert_eq!(state.current_stage, EscalationStage::Stage3DurableNonceStuck);

        // Cannot advance beyond Stage 3
        assert!(!state.advance_stage(1000400));
    }

    #[test]
    fn test_forward_only() {
        let stage = EscalationStage::Stage2PublicRpc3xMaxSlippage;
        assert_eq!(stage.next(), Some(EscalationStage::Stage3DurableNonceStuck));

        let stage3 = EscalationStage::Stage3DurableNonceStuck;
        assert_eq!(stage3.next(), None);
    }

    #[test]
    fn test_needs_new_signature() {
        let mut state = make_state();
        assert!(state.needs_new_signature());

        state.set_stage_signature("sig1".to_string());
        assert!(!state.needs_new_signature());

        // After advancing, needs new signature
        state.advance_stage(1000100);
        assert!(state.needs_new_signature());
    }

    #[test]
    fn test_attempt_tracking() {
        let mut state = make_state();
        // Stage 0 has 2 max attempts
        assert!(state.record_attempt());  // attempt 1 < 2, should retry
        assert!(!state.record_attempt()); // attempt 2 >= 2, should transition
    }

    #[test]
    fn test_signature_audit_trail() {
        let mut state = make_state();
        state.set_stage_signature("sig-stage0".to_string());
        state.advance_stage(1000100);
        state.set_stage_signature("sig-stage1".to_string());

        assert_eq!(state.all_signatures, vec!["sig-stage0", "sig-stage1"]);
        assert_eq!(state.current_stage_signature.as_deref(), Some("sig-stage1"));
    }

    #[test]
    fn test_backoff_values() {
        let mut state = make_state();
        assert_eq!(state.current_backoff_ms(), 100); // Stage 0: [100]

        state.advance_stage(1000100);
        assert_eq!(state.current_backoff_ms(), 200); // Stage 1: [200]

        state.advance_stage(1000200);
        // Stage 2: [200, 400, 800]
        assert_eq!(state.current_backoff_ms(), 200);
        state.record_attempt();
        assert_eq!(state.current_backoff_ms(), 200);
        state.record_attempt();
        assert_eq!(state.current_backoff_ms(), 400);
    }

    #[test]
    fn test_complete_success() {
        let mut state = make_state();
        state.complete_success("final-sig".to_string(), 12345);
        assert!(state.completed);
        assert!(matches!(state.outcome, Some(EscalationOutcome::Success { .. })));
    }

    #[test]
    fn test_is_stuck() {
        let mut state = make_state();
        assert!(!state.is_stuck());
        state.advance_stage(1);
        state.advance_stage(2);
        state.advance_stage(3);
        assert!(state.is_stuck());
    }
}
