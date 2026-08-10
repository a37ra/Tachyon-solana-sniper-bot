//! Smart Money watchlist consumption (TZ §2.5).
//!
//! Modes: tag_only (default) | soft_boost | hard_gate.
//! Hard checks in risk_gate are never weakened.

pub mod consumption;
pub mod mode;
pub mod subscription;

pub use consumption::{apply_entry_policy, apply_size_multiplier, process_hit, process_hit_persist};
pub use mode::{EntryDecision, SmartMoneyMode, DEFAULT_HARD_GATE_MIN_SAMPLE};
pub use subscription::{subscription_wallets, WatchlistCache};
