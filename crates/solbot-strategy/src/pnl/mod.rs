use solbot_common::types::position::Position;
use solbot_common::types::tier::CapitalTier;

/// Fee breakdown for P&L calculation.
#[derive(Debug, Clone, Default)]
pub struct FeeBreakdown {
    /// Priority fees paid in SOL
    pub priority_fees_sol: f64,
    /// Jito bundle tips in SOL
    pub jito_tips_sol: f64,
    /// ATA creation rent if token account was created
    pub ata_rent_sol: f64,
    /// ATA close rent refund when position closes
    pub ata_refund_sol: f64,
}

impl FeeBreakdown {
    /// Total fees paid (sum of priority fees and tips, minus refunds)
    pub fn total_fees(&self) -> f64 {
        self.priority_fees_sol + self.jito_tips_sol + self.ata_rent_sol - self.ata_refund_sol
    }
}

/// Compute net P&L after all fees.
///
/// Formula:
/// - Gross P&L = (exit_price - entry_price) × size_sol
/// - Total Fees = priority_fees + jito_tips + ata_rent - ata_refund
/// - Net P&L = Gross P&L - Total Fees
pub fn compute_pnl_net(
    position: &Position,
    exit_price_sol: f64,
    fees: &FeeBreakdown,
) -> f64 {
    let gross_pnl = (exit_price_sol - position.entry_price_sol) * position.size_sol;
    let total_fees = fees.total_fees();
    
    gross_pnl - total_fees
}

/// Compute P&L percentage.
pub fn compute_pnl_pct(entry_price: f64, exit_price: f64) -> f64 {
    if entry_price == 0.0 {
        0.0
    } else {
        ((exit_price - entry_price) / entry_price) * 100.0
    }
}

/// Estimate default fees based on tier and transaction count.
pub fn estimate_default_fees(
    tier: CapitalTier,
    num_transactions: usize, // buy + exit transactions
) -> FeeBreakdown {
    let base_priority_fee = match tier {
        CapitalTier::Micro => 0.00001,   // ~10k lamports
        CapitalTier::Small => 0.00005,   // ~50k lamports
        CapitalTier::Medium => 0.0001,   // ~100k lamports
        CapitalTier::Large => 0.0005,    // ~500k lamports
    };

    let priority_fees_sol = base_priority_fee * num_transactions as f64;
    let jito_tips_sol = if tier == CapitalTier::Micro {
        0.0 // Micro tier doesn't use Jito bundle
    } else {
        0.0001 * (1.0 + (num_transactions as f64 - 1.0) * 0.5) // ~100k lamports base + scaling
    };

    // ATA rent is only paid once at buy, recovered at exit
    let ata_rent_sol = 0.00203; // ~2.03M lamports for new token account
    let ata_refund_sol = ata_rent_sol;

    FeeBreakdown {
        priority_fees_sol,
        jito_tips_sol,
        ata_rent_sol,
        ata_refund_sol,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pnl_calculation_positive() {
        let position = Position {
            position_id: "test".to_string(),
            mint: "test_mint".to_string(),
            pool_address: "test_pool".to_string(),
            dex_program_id: "test_dex".to_string(),
            tier: CapitalTier::Micro,
            status: Default::default(),
            entry_price_sol: 1.0,
            size_sol: 10.0,
            size_pct_of_capital: 1.0,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 0,
            closed_at_utc: None,
        };

        let fees = FeeBreakdown {
            priority_fees_sol: 0.00002,
            jito_tips_sol: 0.0,
            ata_rent_sol: 0.00203,
            ata_refund_sol: 0.00203,
        };

        let exit_price = 1.5;
        let pnl = compute_pnl_net(&position, exit_price, &fees);
        
        // Gross: (1.5 - 1.0) * 10 = 5.0 SOL
        // Fees: 0.00002 + 0 + 0 = 0.00002 SOL
        // Net: 5.0 - 0.00002 = 4.99998 SOL
        assert!((pnl - 4.99998).abs() < 0.000001);
    }

    #[test]
    fn test_pnl_calculation_negative() {
        let position = Position {
            position_id: "test".to_string(),
            mint: "test_mint".to_string(),
            pool_address: "test_pool".to_string(),
            dex_program_id: "test_dex".to_string(),
            tier: CapitalTier::Micro,
            status: Default::default(),
            entry_price_sol: 1.0,
            size_sol: 10.0,
            size_pct_of_capital: 1.0,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 0,
            closed_at_utc: None,
        };

        let fees = FeeBreakdown {
            priority_fees_sol: 0.00002,
            jito_tips_sol: 0.0,
            ata_rent_sol: 0.00203,
            ata_refund_sol: 0.00203,
        };

        let exit_price = 0.8;
        let pnl = compute_pnl_net(&position, exit_price, &fees);
        
        // Gross: (0.8 - 1.0) * 10 = -2.0 SOL
        // Fees: 0.00002 SOL
        // Net: -2.0 - 0.00002 = -2.00002 SOL
        assert!((pnl - (-2.00002)).abs() < 0.000001);
    }
}
