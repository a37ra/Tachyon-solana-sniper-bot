use solbot_common::types::position::Position;

#[derive(Debug, Clone)]
pub struct FeeBreakdown {
    pub priority_fees_sol: f64,
    pub jito_tips_sol: f64,
    pub ata_rent_sol: f64,
    pub ata_refund_sol: f64,
}

pub fn compute_pnl_net(
    position: &Position,
    exit_price_sol: f64,
    fees: &FeeBreakdown,
) -> f64 {
    let gross_pnl = (exit_price_sol - position.entry_price_sol) * position.size_sol;
    let total_fees = fees.priority_fees_sol + fees.jito_tips_sol + fees.ata_rent_sol - fees.ata_refund_sol;
    
    gross_pnl - total_fees
}
