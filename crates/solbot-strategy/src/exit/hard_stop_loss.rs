use solbot_common::types::position::Position;

/// Default Hard stop loss trigger: -25% from entry price (within -20% .. -30% range).
pub const DEFAULT_HARD_SL_TRIGGER_PCT: f64 = 0.25;

/// Get active Hard SL percentage (supports override via SOLBOT_HARD_SL_PCT, default 0.25 = -25%).
pub fn get_hard_sl_pct() -> f64 {
    std::env::var("SOLBOT_HARD_SL_PCT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| if v > 1.0 { v / 100.0 } else { v })
        .unwrap_or(DEFAULT_HARD_SL_TRIGGER_PCT)
}

/// Evaluate whether the hard stop loss should trigger.
///
/// Returns `true` if `current_price <= sl_price`.
///
/// # Design
/// - Uses tighter of `hard_sl_price_sol` and current active Hard SL threshold (-25%)
/// - Always checked first in exit evaluation (highest priority for capital preservation)
pub fn evaluate_sl(position: &Position, current_price: f64) -> bool {
    if position.entry_price_sol <= 0.0 {
        return false;
    }

    let active_sl_pct = get_hard_sl_pct();
    let computed_sl = position.entry_price_sol * (1.0 - active_sl_pct);

    // If hard_sl_price_sol was stored with the legacy -50% threshold,
    // tighten it to the active threshold (e.g. -25%) so capital is safeguarded.
    let sl_price = if position.hard_sl_price_sol > 0.0 {
        position.hard_sl_price_sol.max(computed_sl)
    } else {
        computed_sl
    };

    current_price <= sl_price
}

/// Compute the hard stop loss price for a given entry price.
pub fn compute_sl_price(entry_price: f64) -> f64 {
    entry_price * (1.0 - get_hard_sl_pct())
}

#[cfg(test)]
mod tests {
    use super::*;
    use solbot_common::types::position::PositionStatus;
    use solbot_common::types::tier::CapitalTier;

    fn make_pos(entry: f64) -> Position {
        Position {
            position_id: "sl-1".to_string(),
            mint: "Mint11111111111111111111111111111111111111".to_string(),
            pool_address: "Pool11111111111111111111111111111111111111".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            tier: CapitalTier::Micro,
            status: PositionStatus::Open,
            entry_price_sol: entry,
            size_sol: 0.5,
            size_pct_of_capital: 0.05,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: compute_sl_price(entry),
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000000,
            closed_at_utc: None,
        }
    }

    #[test]
    fn test_sl_triggers_at_tightened_drop() {
        let pos = make_pos(1.0);
        // Tightened SL price = 0.75 (-25%)
        assert!(!evaluate_sl(&pos, 0.76));
        assert!(evaluate_sl(&pos, 0.75));
        assert!(evaluate_sl(&pos, 0.70));
        assert!(evaluate_sl(&pos, 0.40));
    }

    #[test]
    fn test_sl_not_triggered_in_profit() {
        let pos = make_pos(1.0);
        assert!(!evaluate_sl(&pos, 1.5));
        assert!(!evaluate_sl(&pos, 1.0));
    }

    #[test]
    fn test_sl_edge_at_exact_boundary() {
        let pos = make_pos(2.0);
        // SL price = 2.0 * 0.75 = 1.5
        assert!(evaluate_sl(&pos, 1.50));
        assert!(!evaluate_sl(&pos, 1.501));
    }

    #[test]
    fn test_zero_entry_no_sl() {
        let pos = make_pos(0.0);
        assert!(!evaluate_sl(&pos, 0.0));
    }

    #[test]
    fn test_compute_sl_price() {
        assert!((compute_sl_price(1.0) - 0.75).abs() < 0.001);
        assert!((compute_sl_price(2.0) - 1.50).abs() < 0.001);
    }
}
