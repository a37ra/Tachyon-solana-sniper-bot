//! Consume smart_money_hit events and apply mode policy (TZ §2.5).

use super::mode::{self, EntryDecision, SmartMoneyMode};
use crate::state::smart_money_hits_repo::SmartMoneyHitsRepo;
use solbot_common::types::signal::SmartMoneyHit;
use tracing::{info, warn};

/// Log + optional DB persist. Never logs secrets.
pub fn process_hit(hit: &SmartMoneyHit) {
    info!(
        wallet = %hit.wallet_address,
        mint = %hit.mint,
        pool = %hit.pool_address,
        tx = %hit.tx_signature,
        slot = hit.detected_at_slot,
        "Smart Money hit detected"
    );
}

pub async fn process_hit_persist(
    hit: &SmartMoneyHit,
    repo: Option<&SmartMoneyHitsRepo>,
) -> anyhow::Result<()> {
    process_hit(hit);
    if let Some(repo) = repo {
        let id = format!("{}:{}", hit.tx_signature, hit.mint);
        if let Err(e) = repo
            .insert_hit(
                &id,
                &hit.wallet_address,
                &hit.mint,
                &hit.pool_address,
                &hit.tx_signature,
                hit.detected_at_slot,
            )
            .await
        {
            // Unique tx is fine to ignore
            warn!(error = %e, "smart_money_hits insert (may be dup)");
        }
    }
    Ok(())
}

/// Pre-buy gate: combine mode + whether this mint has a hit.
///
/// **Never** skips risk_gate — caller still runs freeze/LP/simulate after Allow.
pub fn apply_entry_policy(
    mode: SmartMoneyMode,
    mint_has_hit: bool,
    closed_positions_sample: u32,
) -> EntryDecision {
    let d = mode::resolve_entry(
        mode,
        mint_has_hit,
        closed_positions_sample,
        mode::DEFAULT_HARD_GATE_MIN_SAMPLE,
    );
    match &d {
        EntryDecision::Allow { size_multiplier } => {
            info!(
                mode = mode.as_str(),
                mint_has_hit,
                size_multiplier,
                "smart_money entry allow"
            );
        }
        EntryDecision::Block { reason } => {
            warn!(mode = mode.as_str(), %reason, "smart_money entry block");
        }
    }
    d
}

/// Apply size multiplier from entry decision onto a base size (SOL).
pub fn apply_size_multiplier(base_size_sol: f64, decision: &EntryDecision) -> Option<f64> {
    match decision {
        EntryDecision::Allow { size_multiplier } => {
            Some(base_size_sol * mode::clamp_size_multiplier(*size_multiplier))
        }
        EntryDecision::Block { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_hit_does_not_panic() {
        process_hit(&SmartMoneyHit {
            wallet_address: "W".into(),
            mint: "M".into(),
            pool_address: "P".into(),
            tx_signature: "T".into(),
            detected_at_slot: 1,
        });
    }

    #[test]
    fn apply_size_none_on_block() {
        let d = apply_entry_policy(SmartMoneyMode::HardGate, false, 100);
        assert!(apply_size_multiplier(1.0, &d).is_none());
    }

    #[test]
    fn apply_size_boost() {
        let d = apply_entry_policy(SmartMoneyMode::SoftBoost, true, 100);
        let s = apply_size_multiplier(1.0, &d).unwrap();
        assert!(s > 1.0 && s <= 1.25);
    }
}
