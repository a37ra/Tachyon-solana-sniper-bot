pub mod kelly;
pub mod adaptive;

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
    tier: CapitalTier,
    tier_params: &TierParams,
    stats: Option<&TradeStats>,
) -> f64 {
    let base_size = capital * tier_params.sizing.base_size_pct;
    
    let kelly_adjusted = if let Some(stats) = stats {
        if stats.total_trades >= 30 {
            let kelly_fraction = kelly::kelly_fraction(stats.win_rate, stats.avg_win / stats.avg_loss);
            let fractional_kelly = kelly::fractional_kelly(kelly_fraction, tier_params.sizing.kelly.fraction_of_kelly);
            base_size * fractional_kelly
        } else {
            base_size
        }
    } else {
        base_size
    };
    
    let clamped = kelly_adjusted.max(capital * tier_params.sizing.base_size_pct).min(capital * tier_params.sizing.adaptive.size_max_pct);
    
    clamped.min(tier_params.sizing.max_size_sol)
}
