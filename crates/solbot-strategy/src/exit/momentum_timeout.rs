use crate::exit::ExitAction;
use solbot_common::types::position::{Position, PositionStatus};

/// Default momentum evaluation window: 45s (within requested 30..60s range).
pub const DEFAULT_MOMENTUM_TIMEOUT_SECS: u64 = 45;

/// Default required minimum upward impulse: +15% (within requested +15%..+20% range).
pub const DEFAULT_MOMENTUM_MIN_IMPULSE_PCT: f64 = 0.15;

/// Returns the momentum timeout window in seconds (supports SOLBOT_MOMENTUM_TIMEOUT_SECS override).
pub fn get_momentum_timeout_secs() -> u64 {
    std::env::var("SOLBOT_MOMENTUM_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MOMENTUM_TIMEOUT_SECS)
}

/// Returns the minimum required upward impulse percentage (supports SOLBOT_MOMENTUM_MIN_IMPULSE_PCT override).
pub fn get_momentum_min_impulse_pct() -> f64 {
    std::env::var("SOLBOT_MOMENTUM_MIN_IMPULSE_PCT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| if v > 1.0 { v / 100.0 } else { v })
        .unwrap_or(DEFAULT_MOMENTUM_MIN_IMPULSE_PCT)
}

/// Evaluate whether the position should be closed due to lack of initial upward momentum.
///
/// Rule: If the token in the first 30–60s (default 45s) after entry did not give an upward
/// impulse of at least +15%..+20% (default +15%), close immediately at market in breakeven/micro-loss.
pub fn evaluate_momentum(position: &Position, current_price: f64) -> Option<ExitAction> {
    // Only applies to positions that have not yet taken profit (Open or Opening)
    if position.status != PositionStatus::Open && position.status != PositionStatus::Opening {
        return None;
    }

    if position.entry_price_sol <= 0.0 || position.opened_at_utc == 0 {
        return None;
    }

    let now_utc = chrono::Utc::now().timestamp() as u64;
    let elapsed_secs = now_utc.saturating_sub(position.opened_at_utc);
    let timeout_secs = get_momentum_timeout_secs();

    if elapsed_secs < timeout_secs {
        return None;
    }

    let min_impulse_pct = get_momentum_min_impulse_pct();
    let min_required_peak = position.entry_price_sol * (1.0 + min_impulse_pct);

    let peak_price = position
        .trailing_high_water_sol
        .unwrap_or(current_price)
        .max(current_price);

    if peak_price < min_required_peak {
        let peak_multiplier = peak_price / position.entry_price_sol;
        tracing::warn!(
            position_id = %position.position_id,
            elapsed_secs,
            timeout_secs,
            peak_multiplier,
            required_multiplier = %(1.0 + min_impulse_pct),
            current_price,
            "Momentum timeout: token failed to give +{:.1}% impulse in {}s — exiting at breakeven/micro-loss",
            min_impulse_pct * 100.0,
            timeout_secs
        );
        return Some(ExitAction::MomentumTimeout {
            elapsed_secs,
            peak_multiplier,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use solbot_common::types::tier::CapitalTier;

    fn make_test_pos(entry: f64, opened_offset_secs: u64, high_water: Option<f64>) -> Position {
        let now = chrono::Utc::now().timestamp() as u64;
        Position {
            position_id: "test-momentum-pos".to_string(),
            mint: "Mint11111111111111111111111111111111111111".to_string(),
            pool_address: "Pool11111111111111111111111111111111111111".to_string(),
            dex_program_id: "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string(),
            tier: CapitalTier::Small,
            status: PositionStatus::Open,
            entry_price_sol: entry,
            size_sol: 0.01,
            size_pct_of_capital: 0.01,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: entry * 0.75,
            trailing_active: false,
            trailing_high_water_sol: high_water,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: now.saturating_sub(opened_offset_secs),
            closed_at_utc: None,
        }
    }

    #[test]
    fn test_no_exit_before_timeout() {
        // Only 15s elapsed (< 45s)
        let pos = make_test_pos(1.0, 15, Some(1.02));
        assert!(evaluate_momentum(&pos, 1.01).is_none());
    }

    #[test]
    fn test_exit_when_no_impulse_after_timeout() {
        // 50s elapsed (> 45s) and peak was only 1.05 (< 1.15)
        let pos = make_test_pos(1.0, 50, Some(1.05));
        let action = evaluate_momentum(&pos, 0.99);
        assert!(matches!(
            action,
            Some(ExitAction::MomentumTimeout {
                elapsed_secs: 50,
                ..
            })
        ));
    }

    #[test]
    fn test_no_exit_if_impulse_was_achieved() {
        // 55s elapsed (> 45s), but token pumped to 1.18 (> 1.15) earlier
        let pos = make_test_pos(1.0, 55, Some(1.18));
        // Current price pulled back to 1.08, but momentum impulse was achieved
        assert!(evaluate_momentum(&pos, 1.08).is_none());
    }

    #[test]
    fn test_not_applicable_to_tp_states() {
        let mut pos = make_test_pos(1.0, 60, Some(1.02));
        pos.status = PositionStatus::Tp1Filled;
        assert!(evaluate_momentum(&pos, 1.01).is_none());
    }
}
