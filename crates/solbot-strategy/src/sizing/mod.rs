pub mod adaptive;
pub mod kelly;

use solbot_common::config::TierParams;
use solbot_common::types::tier::CapitalTier;

#[derive(Debug, Clone)]
pub struct TradeStats {
    pub win_rate: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub total_trades: u32,
}

#[derive(Debug, Clone)]
pub struct TradeResult {
    pub pnl_pct: f64,
    pub timestamp: u64,
}

pub fn compute_position_size(
    capital: f64,
    _tier: CapitalTier,
    tier_params: &TierParams,
    stats: Option<&TradeStats>,
) -> f64 {
    compute_position_size_with_multiplier(capital, _tier, tier_params, stats, 1.0)
}

/// Same as [`compute_position_size`] then apply smart-money / alpha size multiplier.
/// Multiplier is clamped; result never exceeds `size_max_pct` / `max_size_sol`.
pub fn compute_position_size_with_multiplier(
    capital: f64,
    _tier: CapitalTier,
    tier_params: &TierParams,
    stats: Option<&TradeStats>,
    size_multiplier: f64,
) -> f64 {
    let base_size = capital * tier_params.sizing.base_size_pct;

    let kelly_adjusted = if let Some(stats) = stats {
        if stats.total_trades >= 30 {
            let payoff = if stats.avg_loss > 0.0 {
                stats.avg_win / stats.avg_loss
            } else {
                1.0
            };
            let kelly_fraction = kelly::kelly_fraction(stats.win_rate, payoff);
            let fractional_kelly =
                kelly::fractional_kelly(kelly_fraction, tier_params.sizing.kelly.fraction_of_kelly);
            base_size * fractional_kelly
        } else {
            base_size
        }
    } else {
        base_size
    };

    let mult = size_multiplier.clamp(0.5, 1.25);
    let adjusted = kelly_adjusted * mult;

    let max_pct = capital * tier_params.sizing.adaptive.size_max_pct;
    let min_pct = capital * tier_params.sizing.base_size_pct * 0.5;

    adjusted
        .max(min_pct)
        .min(max_pct)
        .min(tier_params.sizing.max_size_sol)
}
