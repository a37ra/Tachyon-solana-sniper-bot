//! Slot age and duplication filter for incoming DEX signals.
//!
//! Enforces:
//! 1. `max_signal_age_slots` (e.g. 30 slots) — ignores signals that arrived too late
//! 2. Context key deduplication within a sliding time window

use solbot_common::types::Signal;
use chrono::Utc;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_MAX_SIGNAL_AGE_SLOTS: u64 = 2;

#[derive(Clone)]
pub struct SignalFilter {
    max_age_slots: u64,
    seen_mints: Arc<DashMap<String, u64>>,
    dedup_ttl: Duration,
}

impl SignalFilter {
    pub fn new(max_age_slots: u64, dedup_ttl_secs: u64) -> Self {
        Self {
            max_age_slots,
            seen_mints: Arc::new(DashMap::new()),
            dedup_ttl: Duration::from_secs(dedup_ttl_secs),
        }
    }

    /// Check if a signal is valid (fresh and not a duplicate).
    pub fn is_valid_signal(&self, signal: &Signal, current_slot: u64) -> bool {
        // 1. Check slot age if slot info is available
        if current_slot > 0 && signal.slot > 0 {
            if current_slot.saturating_sub(signal.slot) > self.max_age_slots {
                tracing::warn!(
                    mint = %signal.mint,
                    signal_slot = signal.slot,
                    current_slot = current_slot,
                    age = current_slot.saturating_sub(signal.slot),
                    "Signal rejected: slot age exceeds max_signal_age_slots"
                );
                return false;
            }
        }

        // 2. Check deduplication
        let now = Utc::now().timestamp_millis() as u64;
        if let Some(last_seen) = self.seen_mints.get(&signal.mint) {
            if now.saturating_sub(*last_seen) < self.dedup_ttl.as_millis() as u64 {
                tracing::debug!(
                    mint = %signal.mint,
                    "Signal rejected: duplicate mint within TTL window"
                );
                return false;
            }
        }

        // Mark seen
        self.seen_mints.insert(signal.mint.clone(), now);
        true
    }

    /// Periodically purge old items from seen cache.
    pub fn start_cleanup_task(&self) -> tokio::task::JoinHandle<()> {
        let seen = self.seen_mints.clone();
        let ttl_ms = self.dedup_ttl.as_millis() as u64;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let now = Utc::now().timestamp_millis() as u64;
                seen.retain(|_, last_seen| now.saturating_sub(*last_seen) < ttl_ms);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slot_age_filtering() {
        let filter = SignalFilter::new(2, 60);
        let signal = Signal {
            mint: "mint1".to_string(),
            pool_address: "pool1".to_string(),
            dex_program: "raydium".to_string(),
            signal_source: "ws".to_string(),
            slot: 100,
            received_at: 1000,
        };

        // Fresh signal (1 slot old)
        assert!(filter.is_valid_signal(&signal, 101));

        // Stale signal (> 2 slots old)
        let stale_signal = Signal {
            mint: "mint2".to_string(),
            pool_address: "pool2".to_string(),
            dex_program: "raydium".to_string(),
            signal_source: "ws".to_string(),
            slot: 100,
            received_at: 1000,
        };
        assert!(!filter.is_valid_signal(&stale_signal, 105));
    }

    #[test]
    fn test_deduplication() {
        let filter = SignalFilter::new(30, 60);
        let signal1 = Signal {
            mint: "dup_mint".to_string(),
            pool_address: "pool1".to_string(),
            dex_program: "raydium".to_string(),
            signal_source: "ws".to_string(),
            slot: 100,
            received_at: 1000,
        };

        let signal2 = signal1.clone();

        assert!(filter.is_valid_signal(&signal1, 105));
        assert!(!filter.is_valid_signal(&signal2, 106)); // Duplicate
    }
}
