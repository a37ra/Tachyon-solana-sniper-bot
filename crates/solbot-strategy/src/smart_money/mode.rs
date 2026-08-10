//! Smart money modes (TZ §2.5).
//!
//! - `tag_only` (default): log/notify only
//! - `soft_boost`: size multiplier inside size_max_pct clamp — never skips risk_gate
//! - `hard_gate`: require hit to enter — only after min_sample_size closed trades

use std::env;

/// TZ default min closed positions before hard_gate may activate.
pub const DEFAULT_HARD_GATE_MIN_SAMPLE: u32 = 30;

/// Soft-boost size multiplier when a hit is present (≤1.0 boost via larger fraction of base).
/// Applied as `size *= mult` then clamped by sizing layer to size_max_pct.
pub const DEFAULT_SOFT_BOOST_MULT: f64 = 1.15;

/// Mild derate when soft_boost mode but no hit (still allow entry).
pub const DEFAULT_SOFT_NO_HIT_MULT: f64 = 0.90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SmartMoneyMode {
    #[default]
    TagOnly,
    SoftBoost,
    HardGate,
}

impl SmartMoneyMode {
    pub fn parse(mode_str: &str) -> Self {
        match mode_str.to_lowercase().as_str() {
            "soft_boost" | "soft" => Self::SoftBoost,
            "hard_gate" | "hard" => Self::HardGate,
            _ => Self::TagOnly,
        }
    }

    pub fn from_env() -> Self {
        Self::parse(&env::var("SOLBOT_SMART_MONEY_MODE").unwrap_or_else(|_| "tag_only".into()))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TagOnly => "tag_only",
            Self::SoftBoost => "soft_boost",
            Self::HardGate => "hard_gate",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryDecision {
    /// Proceed with buy (optional size mult).
    Allow { size_multiplier: f64 },
    /// Block entry (hard_gate without hit).
    Block { reason: String },
}

/// Resolve entry policy. `closed_positions_sample` = number of closed trades used for calibration gate.
pub fn resolve_entry(
    mode: SmartMoneyMode,
    has_smart_money_hit: bool,
    closed_positions_sample: u32,
    min_sample_for_hard_gate: u32,
) -> EntryDecision {
    match mode {
        SmartMoneyMode::TagOnly => EntryDecision::Allow {
            size_multiplier: 1.0,
        },
        SmartMoneyMode::SoftBoost => {
            let m = if has_smart_money_hit {
                DEFAULT_SOFT_BOOST_MULT
            } else {
                DEFAULT_SOFT_NO_HIT_MULT
            };
            EntryDecision::Allow {
                size_multiplier: m,
            }
        }
        SmartMoneyMode::HardGate => {
            if closed_positions_sample < min_sample_for_hard_gate {
                // Fail-safe: do not enforce hard_gate until sample exists (TZ 2.5).
                tracing::warn!(
                    sample = closed_positions_sample,
                    need = min_sample_for_hard_gate,
                    "hard_gate requested but min_sample_size not met — falling back to tag_only"
                );
                return EntryDecision::Allow {
                    size_multiplier: 1.0,
                };
            }
            if has_smart_money_hit {
                EntryDecision::Allow {
                    size_multiplier: 1.0,
                }
            } else {
                EntryDecision::Block {
                    reason: "smart_money hard_gate: no watchlist hit for mint".into(),
                }
            }
        }
    }
}

/// Clamp multiplier so sizing cannot exceed max via boost alone (boost ≤ this).
pub fn clamp_size_multiplier(mult: f64) -> f64 {
    mult.clamp(0.5, 1.25)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_tag_only() {
        assert_eq!(SmartMoneyMode::parse("nope"), SmartMoneyMode::TagOnly);
        assert_eq!(SmartMoneyMode::parse("HARD_GATE"), SmartMoneyMode::HardGate);
    }

    #[test]
    fn tag_only_always_allows() {
        let d = resolve_entry(SmartMoneyMode::TagOnly, false, 0, 30);
        assert!(matches!(d, EntryDecision::Allow { size_multiplier: 1.0 }));
    }

    #[test]
    fn soft_boost_changes_mult() {
        let hit = resolve_entry(SmartMoneyMode::SoftBoost, true, 100, 30);
        let no = resolve_entry(SmartMoneyMode::SoftBoost, false, 100, 30);
        match (hit, no) {
            (EntryDecision::Allow { size_multiplier: a }, EntryDecision::Allow { size_multiplier: b }) => {
                assert!(a > 1.0);
                assert!(b < 1.0);
            }
            _ => panic!("expected allow"),
        }
    }

    #[test]
    fn hard_gate_blocks_without_hit_when_sample_ok() {
        let d = resolve_entry(SmartMoneyMode::HardGate, false, 50, 30);
        assert!(matches!(d, EntryDecision::Block { .. }));
    }

    #[test]
    fn hard_gate_falls_back_before_sample() {
        let d = resolve_entry(SmartMoneyMode::HardGate, false, 5, 30);
        assert!(matches!(d, EntryDecision::Allow { size_multiplier: 1.0 }));
    }

    #[test]
    fn hard_gate_allows_with_hit() {
        let d = resolve_entry(SmartMoneyMode::HardGate, true, 50, 30);
        assert!(matches!(d, EntryDecision::Allow { .. }));
    }
}
