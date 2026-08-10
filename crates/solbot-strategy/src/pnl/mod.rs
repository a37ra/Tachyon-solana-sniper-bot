//! PnL net accounting (TZ §4.5).
//!
//! `realized_pnl_net_sol = gross_pnl - priority_fees - jito_tips - ata_rent + ata_refund`

use solbot_common::types::position::Position;
use solbot_common::types::tier::CapitalTier;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Fee breakdown for a single position lifecycle (buy + exits).
#[derive(Debug, Clone, Default)]
pub struct FeeBreakdown {
    pub priority_fees_sol: f64,
    pub jito_tips_sol: f64,
    pub ata_rent_sol: f64,
    pub ata_refund_sol: f64,
}

impl FeeBreakdown {
    pub fn total_fees(&self) -> f64 {
        self.priority_fees_sol + self.jito_tips_sol + self.ata_rent_sol - self.ata_refund_sol
    }

    pub fn add_priority_lamports(&mut self, lamports: u64) {
        self.priority_fees_sol += lamports_to_sol(lamports);
    }

    pub fn add_jito_tip_lamports(&mut self, lamports: u64) {
        self.jito_tips_sol += lamports_to_sol(lamports);
    }

    pub fn add_ata_rent_lamports(&mut self, lamports: u64) {
        self.ata_rent_sol += lamports_to_sol(lamports);
    }

    pub fn add_ata_refund_lamports(&mut self, lamports: u64) {
        self.ata_refund_sol += lamports_to_sol(lamports);
    }
}

#[inline]
pub fn lamports_to_sol(lamports: u64) -> f64 {
    lamports as f64 / 1_000_000_000.0
}

#[inline]
pub fn sol_to_lamports(sol: f64) -> u64 {
    (sol.max(0.0) * 1_000_000_000.0).round() as u64
}

/// Fallback ATA rent-exempt estimate; prefer RPC in live.
pub const DEFAULT_ATA_RENT_LAMPORTS: u64 = 2_039_280;

/// In-memory fee ledger keyed by `position_id`.
#[derive(Debug, Default, Clone)]
pub struct PositionFeeLedger {
    inner: Arc<Mutex<HashMap<String, FeeBreakdown>>>,
}

impl PositionFeeLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn track_priority_fee(&self, position_id: &str, lamports: u64) {
        self.inner
            .lock()
            .expect("fee ledger")
            .entry(position_id.to_string())
            .or_default()
            .add_priority_lamports(lamports);
    }

    pub fn track_jito_tip(&self, position_id: &str, lamports: u64) {
        self.inner
            .lock()
            .expect("fee ledger")
            .entry(position_id.to_string())
            .or_default()
            .add_jito_tip_lamports(lamports);
    }

    pub fn track_ata_rent(&self, position_id: &str, lamports: u64) {
        self.inner
            .lock()
            .expect("fee ledger")
            .entry(position_id.to_string())
            .or_default()
            .add_ata_rent_lamports(lamports);
    }

    pub fn track_ata_refund(&self, position_id: &str, lamports: u64) {
        self.inner
            .lock()
            .expect("fee ledger")
            .entry(position_id.to_string())
            .or_default()
            .add_ata_refund_lamports(lamports);
    }

    pub fn get(&self, position_id: &str) -> FeeBreakdown {
        self.inner
            .lock()
            .expect("fee ledger")
            .get(position_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn take(&self, position_id: &str) -> FeeBreakdown {
        self.inner
            .lock()
            .expect("fee ledger")
            .remove(position_id)
            .unwrap_or_default()
    }
}

/// Gross PnL in SOL: return on `size_sol` notional from entry→exit mark.
pub fn compute_gross_pnl_sol(entry_price_sol: f64, exit_price_sol: f64, size_sol: f64) -> f64 {
    if entry_price_sol <= 0.0 || size_sol == 0.0 {
        return 0.0;
    }
    ((exit_price_sol / entry_price_sol) - 1.0) * size_sol
}

/// Net P&L after fees (TZ §4.5) for a full-size close mark.
pub fn compute_pnl_net(position: &Position, exit_price_sol: f64, fees: &FeeBreakdown) -> f64 {
    let size = position.size_sol * position.remaining_token_pct.clamp(0.0, 1.0);
    let size = if size <= 0.0 {
        position.size_sol
    } else {
        size
    };
    compute_gross_pnl_sol(position.entry_price_sol, exit_price_sol, size) - fees.total_fees()
}

/// Write `realized_pnl_net_sol` on position; returns net.
pub fn finalize_realized_pnl(
    position: &mut Position,
    exit_price_sol: f64,
    fees: &FeeBreakdown,
) -> f64 {
    let net = compute_pnl_net(position, exit_price_sol, fees);
    position.realized_pnl_net_sol = net;
    net
}

pub fn compute_pnl_pct(entry_price: f64, exit_price: f64) -> f64 {
    if entry_price == 0.0 {
        0.0
    } else {
        ((exit_price - entry_price) / entry_price) * 100.0
    }
}

/// Paper/fallback fee estimate when exact lamports were not recorded.
pub fn estimate_default_fees(tier: CapitalTier, num_transactions: usize) -> FeeBreakdown {
    let base_priority_fee = match tier {
        CapitalTier::Micro => 0.00001,
        CapitalTier::Small => 0.00005,
        CapitalTier::Medium => 0.0001,
        CapitalTier::Large => 0.0005,
    };
    let priority_fees_sol = base_priority_fee * num_transactions.max(1) as f64;
    let jito_tips_sol = if matches!(tier, CapitalTier::Micro) {
        0.0
    } else {
        0.0001 * (1.0 + (num_transactions.saturating_sub(1) as f64) * 0.5)
    };
    let ata_rent_sol = lamports_to_sol(DEFAULT_ATA_RENT_LAMPORTS);
    FeeBreakdown {
        priority_fees_sol,
        jito_tips_sol,
        ata_rent_sol,
        ata_refund_sol: ata_rent_sol,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solbot_common::types::position::PositionStatus;

    fn pos(entry: f64, size: f64) -> Position {
        Position {
            position_id: "test".into(),
            mint: "m".into(),
            pool_address: "p".into(),
            dex_program_id: "d".into(),
            tier: CapitalTier::Micro,
            status: PositionStatus::Open,
            entry_price_sol: entry,
            size_sol: size,
            size_pct_of_capital: 0.05,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: entry * 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 0,
            closed_at_utc: None,
        }
    }

    #[test]
    fn pnl_positive_net_of_fees() {
        let position = pos(1.0, 10.0);
        let fees = FeeBreakdown {
            priority_fees_sol: 0.00002,
            jito_tips_sol: 0.0,
            ata_rent_sol: 0.00203,
            ata_refund_sol: 0.00203,
        };
        let pnl = compute_pnl_net(&position, 1.5, &fees);
        assert!((pnl - 4.99998).abs() < 1e-6);
    }

    #[test]
    fn pnl_negative() {
        let position = pos(1.0, 10.0);
        let fees = FeeBreakdown {
            priority_fees_sol: 0.00002,
            jito_tips_sol: 0.0,
            ata_rent_sol: 0.002,
            ata_refund_sol: 0.002,
        };
        assert!((compute_pnl_net(&position, 0.8, &fees) - (-2.00002)).abs() < 1e-6);
    }

    #[test]
    fn ledger_tracks_priority_and_tip() {
        let ledger = PositionFeeLedger::new();
        ledger.track_priority_fee("p1", 10_000);
        ledger.track_jito_tip("p1", 100_000);
        ledger.track_ata_rent("p1", DEFAULT_ATA_RENT_LAMPORTS);
        ledger.track_ata_refund("p1", DEFAULT_ATA_RENT_LAMPORTS);
        let f = ledger.take("p1");
        assert!((f.priority_fees_sol - 0.00001).abs() < 1e-12);
        assert!((f.jito_tips_sol - 0.0001).abs() < 1e-12);
        assert!((f.total_fees() - 0.00011).abs() < 1e-9);
    }

    #[test]
    fn finalize_writes_realized_field() {
        let mut p = pos(1.0, 1.0);
        let net = finalize_realized_pnl(&mut p, 2.0, &FeeBreakdown::default());
        assert!((net - 1.0).abs() < 1e-9);
        assert!((p.realized_pnl_net_sol - 1.0).abs() < 1e-9);
    }

    #[test]
    fn estimate_micro_no_jito() {
        let f = estimate_default_fees(CapitalTier::Micro, 2);
        assert_eq!(f.jito_tips_sol, 0.0);
        assert!(f.priority_fees_sol > 0.0);
    }
}
