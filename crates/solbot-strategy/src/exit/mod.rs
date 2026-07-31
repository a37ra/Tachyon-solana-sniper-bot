pub mod take_profit_ladder;
pub mod trailing_stop;
pub mod hard_stop_loss;
pub mod escalation;

use solbot_common::types::position::Position;

/// Represents a specific take-profit level in the TP ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpLevel {
    /// +50% from entry, sell 30%
    Tp1,
    /// +100% from entry, sell 30%
    Tp2,
    /// +200% from entry, sell 20%
    Tp3,
}

impl TpLevel {
    /// Returns the percentage of the *original* position to sell at this TP level.
    pub fn sell_pct(&self) -> f64 {
        match self {
            TpLevel::Tp1 => 0.30,
            TpLevel::Tp2 => 0.30,
            TpLevel::Tp3 => 0.20,
        }
    }

    /// Returns the profit trigger multiplier (e.g., 1.5 for +50%).
    pub fn trigger_multiplier(&self) -> f64 {
        match self {
            TpLevel::Tp1 => 1.5,
            TpLevel::Tp2 => 2.0,
            TpLevel::Tp3 => 3.0,
        }
    }
}

/// Action to take when an exit condition is triggered.
#[derive(Debug, Clone)]
pub enum ExitAction {
    /// Take profit at the given level, selling the specified percentage.
    TakeProfit {
        level: TpLevel,
        /// Fraction of *remaining* position to sell (adjusted for prior TPs).
        sell_fraction_of_remaining: f64,
    },
    /// Trailing stop triggered — sell all remaining tokens.
    TrailingStop {
        trigger_price: f64,
        high_water_mark: f64,
    },
    /// Hard stop loss at -50% — sell 100% and start escalation.
    HardStopLoss,
    /// Emergency exit triggered externally (circuit breaker, kill switch).
    Emergency {
        reason: String,
    },
}

/// Evaluate whether the current price triggers any exit action for the given position.
///
/// Priority order:
/// 1. Hard stop loss (most urgent — capital preservation)
/// 2. Trailing stop (if active)
/// 3. Take profit ladder (TP1 → TP2 → TP3)
///
/// Returns `None` if no exit condition is met.
pub fn evaluate_exit(position: &Position, current_price: f64) -> Option<ExitAction> {
    // 1. Hard stop loss — always checked first (capital preservation)
    if hard_stop_loss::evaluate_sl(position, current_price) {
        tracing::warn!(
            position_id = %position.position_id,
            entry_price = %position.entry_price_sol,
            current_price = %current_price,
            "Hard stop loss triggered at -50%"
        );
        return Some(ExitAction::HardStopLoss);
    }

    // 2. Trailing stop — checked if active (TP1 already hit)
    if position.trailing_active {
        if let Some(trigger_price) = trailing_stop::evaluate_trailing(position, current_price) {
            tracing::info!(
                position_id = %position.position_id,
                trigger_price = %trigger_price,
                high_water = ?position.trailing_high_water_sol,
                "Trailing stop triggered"
            );
            return Some(ExitAction::TrailingStop {
                trigger_price,
                high_water_mark: position.trailing_high_water_sol.unwrap_or(current_price),
            });
        }
    }

    // 3. Take profit ladder — check based on current status
    if let Some(tp_level) = take_profit_ladder::evaluate_tp(position, current_price) {
        let sell_fraction = compute_sell_fraction_of_remaining(&tp_level, position);
        tracing::info!(
            position_id = %position.position_id,
            tp_level = ?tp_level,
            sell_fraction = %sell_fraction,
            current_price = %current_price,
            "Take profit triggered"
        );
        return Some(ExitAction::TakeProfit {
            level: tp_level,
            sell_fraction_of_remaining: sell_fraction,
        });
    }

    None
}

/// Compute what fraction of the *remaining* tokens to sell for this TP level.
///
/// TP levels are defined as percentages of the *original* position:
/// - TP1: 30% of original → 30% / remaining_token_pct
/// - TP2: 30% of original → 30% / remaining_token_pct
/// - TP3: 20% of original → 20% / remaining_token_pct
///
/// remaining_token_pct tracks what fraction of original tokens we still hold.
fn compute_sell_fraction_of_remaining(tp_level: &TpLevel, position: &Position) -> f64 {
    if position.remaining_token_pct <= 0.0 {
        return 0.0;
    }

    let sell_pct_of_original = tp_level.sell_pct();
    let fraction = sell_pct_of_original / position.remaining_token_pct;

    // Clamp to [0, 1] — can't sell more than we hold
    fraction.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solbot_common::types::position::PositionStatus;
    use solbot_common::types::tier::CapitalTier;

    fn make_position(entry_price: f64, status: PositionStatus, remaining_pct: f64) -> Position {
        Position {
            position_id: "test-pos-1".to_string(),
            mint: "TestMint111111111111111111111111111111111".to_string(),
            pool_address: "TestPool111111111111111111111111111111111".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            tier: CapitalTier::Micro,
            status,
            entry_price_sol: entry_price,
            size_sol: 0.5,
            size_pct_of_capital: 0.05,
            remaining_token_pct: remaining_pct,
            hard_sl_price_sol: entry_price * 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000000,
            closed_at_utc: None,
        }
    }

    #[test]
    fn test_hard_sl_takes_priority() {
        let pos = make_position(1.0, PositionStatus::Open, 1.0);
        // Price at 40% of entry → below -50% SL
        let action = evaluate_exit(&pos, 0.4);
        assert!(matches!(action, Some(ExitAction::HardStopLoss)));
    }

    #[test]
    fn test_tp1_triggers_at_50pct_gain() {
        let pos = make_position(1.0, PositionStatus::Open, 1.0);
        // Price at +60% → triggers TP1 (+50%)
        let action = evaluate_exit(&pos, 1.6);
        assert!(matches!(action, Some(ExitAction::TakeProfit { level: TpLevel::Tp1, .. })));
    }

    #[test]
    fn test_tp2_triggers_after_tp1() {
        let pos = make_position(1.0, PositionStatus::Tp1Filled, 0.7);
        // Price at +120% → triggers TP2 (+100%)
        let action = evaluate_exit(&pos, 2.2);
        assert!(matches!(action, Some(ExitAction::TakeProfit { level: TpLevel::Tp2, .. })));
    }

    #[test]
    fn test_no_exit_in_range() {
        let pos = make_position(1.0, PositionStatus::Open, 1.0);
        // Price at +20% → no exit conditions met
        let action = evaluate_exit(&pos, 1.2);
        assert!(action.is_none());
    }

    #[test]
    fn test_sell_fraction_of_remaining() {
        let pos = make_position(1.0, PositionStatus::Open, 1.0);
        assert!((compute_sell_fraction_of_remaining(&TpLevel::Tp1, &pos) - 0.30).abs() < 0.001);

        let pos_after_tp1 = make_position(1.0, PositionStatus::Tp1Filled, 0.7);
        let fraction = compute_sell_fraction_of_remaining(&TpLevel::Tp2, &pos_after_tp1);
        // 0.30 / 0.7 ≈ 0.4286
        assert!((fraction - 0.4286).abs() < 0.01);
    }
}
