use super::TpLevel;
use solbot_common::types::position::{Position, PositionStatus};

/// Evaluate whether the current price triggers a take-profit level
/// based on the position's current status.
///
/// TP Ladder (from ТЗ 4.4):
/// - TP1: +50% from entry → sell 30% of original position
/// - TP2: +100% from entry → sell 30% of original position
/// - TP3: +200% from entry → sell 20% of original position
/// - Remaining 20% = moon bag (held indefinitely or until SL/trailing)
///
/// Returns `None` if no TP level is triggered, or if the position
/// has already filled all TP levels.
pub fn evaluate_tp(position: &Position, current_price: f64) -> Option<TpLevel> {
    if position.entry_price_sol <= 0.0 {
        return None;
    }

    let profit_multiplier = current_price / position.entry_price_sol;

    match position.status {
        // No TPs filled yet — check TP1
        PositionStatus::Open | PositionStatus::Opening => {
            if profit_multiplier >= TpLevel::Tp1.trigger_multiplier() {
                Some(TpLevel::Tp1)
            } else {
                None
            }
        }
        // TP1 filled — check TP2
        PositionStatus::Tp1Filled => {
            if profit_multiplier >= TpLevel::Tp2.trigger_multiplier() {
                Some(TpLevel::Tp2)
            } else {
                None
            }
        }
        // TP2 filled — check TP3
        PositionStatus::Tp2Filled => {
            if profit_multiplier >= TpLevel::Tp3.trigger_multiplier() {
                Some(TpLevel::Tp3)
            } else {
                None
            }
        }
        // TP3 filled — moon bag, no more TPs
        PositionStatus::Tp3Filled => None,
        // Position is closing/closed/stuck — no TPs
        PositionStatus::Closing | PositionStatus::Closed | PositionStatus::StuckManualInterventionRequired => None,
    }
}

/// Given a TP level that was just filled, compute the new PositionStatus.
pub fn next_status_after_tp(level: TpLevel) -> PositionStatus {
    match level {
        TpLevel::Tp1 => PositionStatus::Tp1Filled,
        TpLevel::Tp2 => PositionStatus::Tp2Filled,
        TpLevel::Tp3 => PositionStatus::Tp3Filled,
    }
}

/// Compute the new remaining_token_pct after a TP fill.
///
/// Each TP level sells a percentage of the *original* position:
/// - After TP1 (sell 30%): remaining = original × 0.70
/// - After TP2 (sell 30%): remaining = original × 0.40
/// - After TP3 (sell 20%): remaining = original × 0.20 (moon bag)
pub fn remaining_after_tp(level: TpLevel) -> f64 {
    match level {
        TpLevel::Tp1 => 0.70,
        TpLevel::Tp2 => 0.40,
        TpLevel::Tp3 => 0.20,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solbot_common::types::tier::CapitalTier;

    fn make_pos(entry: f64, status: PositionStatus) -> Position {
        Position {
            position_id: "test-1".to_string(),
            mint: "Mint11111111111111111111111111111111111111".to_string(),
            pool_address: "Pool11111111111111111111111111111111111111".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            tier: CapitalTier::Micro,
            status,
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
    fn test_tp1_triggers_at_50pct() {
        let pos = make_pos(1.0, PositionStatus::Open);
        assert_eq!(evaluate_tp(&pos, 1.49), None);
        assert_eq!(evaluate_tp(&pos, 1.50), Some(TpLevel::Tp1));
        assert_eq!(evaluate_tp(&pos, 2.00), Some(TpLevel::Tp1)); // higher also triggers TP1
    }

    #[test]
    fn test_tp2_requires_tp1_filled() {
        let pos = make_pos(1.0, PositionStatus::Open);
        // Even at +120%, returns TP1 since TP1 hasn't been filled
        assert_eq!(evaluate_tp(&pos, 2.20), Some(TpLevel::Tp1));

        let pos_tp1 = make_pos(1.0, PositionStatus::Tp1Filled);
        assert_eq!(evaluate_tp(&pos_tp1, 1.90), None);
        assert_eq!(evaluate_tp(&pos_tp1, 2.00), Some(TpLevel::Tp2));
    }

    #[test]
    fn test_tp3_requires_tp2_filled() {
        let pos = make_pos(1.0, PositionStatus::Tp2Filled);
        assert_eq!(evaluate_tp(&pos, 2.90), None);
        assert_eq!(evaluate_tp(&pos, 3.00), Some(TpLevel::Tp3));
    }

    #[test]
    fn test_moon_bag_no_more_tps() {
        let pos = make_pos(1.0, PositionStatus::Tp3Filled);
        assert_eq!(evaluate_tp(&pos, 10.0), None);
    }

    #[test]
    fn test_closed_position_no_tps() {
        let pos = make_pos(1.0, PositionStatus::Closed);
        assert_eq!(evaluate_tp(&pos, 5.0), None);
    }

    #[test]
    fn test_remaining_after_tp() {
        assert!((remaining_after_tp(TpLevel::Tp1) - 0.70).abs() < 0.001);
        assert!((remaining_after_tp(TpLevel::Tp2) - 0.40).abs() < 0.001);
        assert!((remaining_after_tp(TpLevel::Tp3) - 0.20).abs() < 0.001);
    }

    #[test]
    fn test_zero_entry_price() {
        let pos = make_pos(0.0, PositionStatus::Open);
        assert_eq!(evaluate_tp(&pos, 1.0), None);
    }
}
