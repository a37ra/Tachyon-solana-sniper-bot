use solbot_common::types::position::Position;

/// The default percentage drop from high water mark that triggers the trailing stop.
/// 25% trailing distance means we sell if price drops to 75% of the high water mark.
const TRAILING_STOP_DISTANCE_PCT: f64 = 0.25;

fn env_pct(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|v| if v > 1.0 { v / 100.0 } else { v })
        .unwrap_or(default)
}

pub fn get_trailing_distance_pct() -> f64 {
    env_pct("SOLBOT_TRAILING_DISTANCE_PCT", TRAILING_STOP_DISTANCE_PCT).clamp(0.01, 0.90)
}

pub fn get_trailing_breakeven_floor_pct() -> f64 {
    env_pct("SOLBOT_TRAILING_BREAKEVEN_FLOOR_PCT", 0.0).max(0.0)
}

pub fn get_trailing_activation_pct() -> f64 {
    env_pct("SOLBOT_TRAILING_ACTIVATION_PCT", 0.50).max(0.01)
}

/// Evaluate whether the trailing stop should trigger for the given position.
///
/// Trailing stop behavior (from ТЗ 4.4):
/// - **Activation**: When TP1 fires (+50%) or early activation threshold is reached
/// - **Trail distance**: configurable from the high water mark (default 25%)
/// - **Breakeven floor**: optional floor above entry once trailing is active
/// - **Applies to**: Remaining position after TP ladder fills
///
/// Returns `Some(trigger_price)` if the trailing stop should fire,
/// meaning `current_price <= trigger_price`.
/// Returns `None` if the trailing stop is not triggered.
///
/// # Note
/// This function does NOT update the high water mark — that should be done
/// by the caller (mint_actor) on every PriceTick when trailing is active.
pub fn evaluate_trailing(position: &Position, current_price: f64) -> Option<f64> {
    // Trailing must be active (set when TP1 fires or early activation hits)
    if !position.trailing_active {
        return None;
    }

    let high_water = match position.trailing_high_water_sol {
        Some(hw) if hw > 0.0 => hw,
        _ => return None, // No high water mark yet
    };

    let raw_trigger = high_water * (1.0 - get_trailing_distance_pct());
    let floor_pct = get_trailing_breakeven_floor_pct();
    let trigger_price = if floor_pct > 0.0 && position.entry_price_sol > 0.0 {
        raw_trigger.max(position.entry_price_sol * (1.0 + floor_pct))
    } else {
        raw_trigger
    };

    if current_price <= trigger_price {
        Some(trigger_price)
    } else {
        None
    }
}

/// Compute the updated high water mark given the current price.
///
/// The high water mark is the maximum price observed since trailing was activated.
/// Only call this when `trailing_active == true`.
pub fn update_high_water(current_high_water: Option<f64>, current_price: f64) -> f64 {
    match current_high_water {
        Some(hw) => hw.max(current_price),
        None => current_price,
    }
}

/// Check whether trailing stop should be activated based on position status
/// and current price. Trailing stop activates when TP1 fires or early activation threshold is reached.
pub fn should_activate_trailing(position: &Position, current_price: f64) -> bool {
    // Don't activate if already active
    if position.trailing_active {
        return false;
    }

    // Activate when price reaches activation threshold (default +50%, overridable via SOLBOT_TRAILING_ACTIVATION_PCT)
    if position.entry_price_sol <= 0.0 {
        return false;
    }

    current_price >= position.entry_price_sol * (1.0 + get_trailing_activation_pct())
}

#[cfg(test)]
mod tests {
    use super::*;
    use solbot_common::types::position::PositionStatus;
    use solbot_common::types::tier::CapitalTier;

    fn make_trailing_pos(entry: f64, high_water: f64) -> Position {
        Position {
            position_id: "trail-1".to_string(),
            mint: "Mint11111111111111111111111111111111111111".to_string(),
            pool_address: "Pool11111111111111111111111111111111111111".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            tier: CapitalTier::Micro,
            status: PositionStatus::Tp1Filled,
            entry_price_sol: entry,
            size_sol: 0.5,
            size_pct_of_capital: 0.05,
            remaining_token_pct: 0.70,
            hard_sl_price_sol: entry * 0.5,
            trailing_active: true,
            trailing_high_water_sol: Some(high_water),
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000000,
            closed_at_utc: None,
        }
    }

    #[test]
    fn test_trailing_triggers_at_25pct_drop() {
        let pos = make_trailing_pos(1.0, 2.0);
        // High water = 2.0, trigger at 2.0 * 0.75 = 1.5
        assert!(evaluate_trailing(&pos, 1.5).is_some());
        assert!(evaluate_trailing(&pos, 1.4).is_some());
        assert!(evaluate_trailing(&pos, 1.51).is_none());
    }

    #[test]
    fn test_trailing_not_active() {
        let mut pos = make_trailing_pos(1.0, 2.0);
        pos.trailing_active = false;
        assert!(evaluate_trailing(&pos, 1.0).is_none());
    }

    #[test]
    fn test_trailing_no_high_water() {
        let mut pos = make_trailing_pos(1.0, 2.0);
        pos.trailing_high_water_sol = None;
        assert!(evaluate_trailing(&pos, 1.0).is_none());
    }

    #[test]
    fn test_update_high_water() {
        assert_eq!(update_high_water(None, 1.5), 1.5);
        assert_eq!(update_high_water(Some(1.5), 2.0), 2.0);
        assert_eq!(update_high_water(Some(2.0), 1.5), 2.0); // no decrease
    }

    #[test]
    fn test_should_activate_at_tp1() {
        let mut pos = make_trailing_pos(1.0, 2.0);
        pos.trailing_active = false;
        assert!(should_activate_trailing(&pos, 1.5));
        assert!(!should_activate_trailing(&pos, 1.49));
    }

    #[test]
    fn test_no_double_activation() {
        let pos = make_trailing_pos(1.0, 2.0);
        // Already active
        assert!(!should_activate_trailing(&pos, 3.0));
    }
}
