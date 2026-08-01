//! Signal validation for age and other checks.
//!
//! Validates signals based on slot age, duplicate detection, and other criteria.

use anyhow::Result;
use solbot_common::types::Signal;
use tracing::{warn, info};

/// Maximum allowed signal age in slots.
/// Signals older than this are rejected to avoid acting on stale data.
pub const MAX_SIGNAL_AGE_SLOTS: u64 = 30;

/// Signal validator configuration.
#[derive(Debug, Clone)]
pub struct SignalValidatorConfig {
    /// Maximum allowed signal age in slots
    pub max_signal_age_slots: u64,
}

impl Default for SignalValidatorConfig {
    fn default() -> Self {
        Self {
            max_signal_age_slots: MAX_SIGNAL_AGE_SLOTS,
        }
    }
}

/// Signal validator.
pub struct SignalValidator {
    config: SignalValidatorConfig,
    current_slot: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl SignalValidator {
    /// Create a new signal validator.
    pub fn new(config: SignalValidatorConfig) -> Self {
        Self {
            config,
            current_slot: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Create with default configuration.
    pub fn with_default() -> Self {
        Self::new(SignalValidatorConfig::default())
    }

    /// Update the current slot (should be called regularly from slot updates).
    pub fn update_current_slot(&self, slot: u64) {
        self.current_slot.store(slot, std::sync::atomic::Ordering::SeqCst);
    }

    /// Get the current slot.
    pub fn get_current_slot(&self) -> u64 {
        self.current_slot.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Validate a signal based on slot age.
    ///
    /// Returns `Ok(true)` if the signal is valid, `Ok(false)` if it should be filtered out,
    /// or `Err` if there's an error.
    pub fn validate_signal(&self, signal: &Signal) -> Result<bool> {
        let current_slot = self.get_current_slot();

        // If we haven't received any slot updates yet, accept the signal
        // (this can happen during startup)
        if current_slot == 0 {
            info!(
                mint = %signal.mint,
                signal_slot = signal.slot,
                "No current slot available, accepting signal"
            );
            return Ok(true);
        }

        // Calculate signal age
        let slot_age = current_slot.saturating_sub(signal.slot);

        // Check if signal is too old
        if slot_age > self.config.max_signal_age_slots {
            warn!(
                mint = %signal.mint,
                signal_slot = signal.slot,
                current_slot,
                slot_age,
                max_age = self.config.max_signal_age_slots,
                "Signal rejected: too old"
            );
            return Ok(false);
        }

        // Signal is valid
        info!(
            mint = %signal.mint,
            signal_slot = signal.slot,
            current_slot,
            slot_age,
            "Signal accepted: age within limit"
        );
        Ok(true)
    }

    /// Validate multiple signals, returning only the valid ones.
    pub fn validate_signals(&self, signals: &[Signal]) -> Vec<Signal> {
        signals
            .iter()
            .filter(|signal| {
                match self.validate_signal(signal) {
                    Ok(valid) => valid,
                    Err(e) => {
                        warn!(error = %e, mint = %signal.mint, "Error validating signal, rejecting");
                        false
                    }
                }
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_validator_default_config() {
        let validator = SignalValidator::with_default();
        assert_eq!(validator.config.max_signal_age_slots, 30);
    }

    #[test]
    fn test_signal_validator_slot_update() {
        let validator = SignalValidator::with_default();
        validator.update_current_slot(100);
        assert_eq!(validator.get_current_slot(), 100);
    }

    #[test]
    fn test_signal_validation_accepts_fresh_signal() {
        let validator = SignalValidator::with_default();
        validator.update_current_slot(100);

        let signal = Signal {
            mint: "test_mint".to_string(),
            pool_address: "test_pool".to_string(),
            dex_program: "test_program".to_string(),
            signal_source: "test".to_string(),
            slot: 95, // 5 slots old
            received_at: 0,
        };

        let result = validator.validate_signal(&signal).unwrap();
        assert!(result);
    }

    #[test]
    fn test_signal_validation_rejects_old_signal() {
        let validator = SignalValidator::with_default();
        validator.update_current_slot(100);

        let signal = Signal {
            mint: "test_mint".to_string(),
            pool_address: "test_pool".to_string(),
            dex_program: "test_program".to_string(),
            signal_source: "test".to_string(),
            slot: 50, // 50 slots old (exceeds 30)
            received_at: 0,
        };

        let result = validator.validate_signal(&signal).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_signal_validation_accepts_when_no_current_slot() {
        let validator = SignalValidator::with_default();
        // Don't update current slot (stays at 0)

        let signal = Signal {
            mint: "test_mint".to_string(),
            pool_address: "test_pool".to_string(),
            dex_program: "test_program".to_string(),
            signal_source: "test".to_string(),
            slot: 50,
            received_at: 0,
        };

        let result = validator.validate_signal(&signal).unwrap();
        assert!(result); // Should accept when no current slot
    }
}
