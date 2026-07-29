use solbot_common::types::position::Position;

/// Hard stop loss trigger: -50% from entry price (from ТЗ 4.4).
///
/// When triggered:
/// 1. Sell 100% of remaining position
/// 2. Start the 4-stage escalation state machine
///
/// This is the most critical exit — capital preservation.
const HARD_SL_TRIGGER_PCT: f64 = 0.50;

/// Evaluate whether the hard stop loss should trigger.
///
/// Returns `true` if `current_price <= entry_price * (1 - HARD_SL_TRIGGER_PCT)`,
/// i.e., the price has dropped 50% or more from entry.
///
/// # Design
/// - Uses `hard_sl_price_sol` from Position if set, otherwise computes from entry price
/// - Always checked first in exit evaluation (highest priority)
pub fn evaluate_sl(position: &Position, current_price: f64) -> bool {
    if position.entry_price_sol <= 0.0 {
        return false;
    }

    // Use pre-computed SL price if available, otherwise compute
    let sl_price = if position.hard_sl_price_sol > 0.0 {
        position.hard_sl_price_sol
    } else {
        position.entry_price_sol * (1.0 - HARD_SL_TRIGGER_PCT)
    };

    current_price <= sl_price
}

/// Compute the hard stop loss price for a given entry price.
pub fn compute_sl_price(entry_price: f64) -> f64 {
    entry_price * (1.0 - HARD_SL_TRIGGER_PCT)
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
            hard_sl_price_sol: entry * 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000000,
            closed_at_utc: None,
        }
    }

    #[test]
    fn test_sl_triggers_at_50pct_drop() {
        let pos = make_pos(1.0);
        // SL price = 0.5
        assert!(!evaluate_sl(&pos, 0.51));
        assert!(evaluate_sl(&pos, 0.50));
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
        // SL price = 1.0
        assert!(evaluate_sl(&pos, 1.0));
        assert!(!evaluate_sl(&pos, 1.001));
    }

    #[test]
    fn test_zero_entry_no_sl() {
        let pos = make_pos(0.0);
        assert!(!evaluate_sl(&pos, 0.0));
    }

    #[test]
    fn test_compute_sl_price() {
        assert!((compute_sl_price(1.0) - 0.5).abs() < 0.001);
        assert!((compute_sl_price(2.5) - 1.25).abs() < 0.001);
    }
}
