//! Alpha Layer (TZ §8 & `docs/alpha/ALPHA_LAYER_FORMULAS.md`).
//!
//! Implements:
//! - On-chain signals (`dev_buy_pct`, `bundler_pct`, `funder_cluster_pct`, `fresh_wallet_ratio`, `Concentration Multiplier`)
//! - Dev History Wilson lower bound (95% CI) and automated post-launch labeling rule
//! - Composite Alpha Gate (tier-specific hard blocks + weighted soft risk score)
//! - Size derating (`size_multiplier`) and Probe-Scale EV breakeven (`p* = delta / (L + delta)`)

use solbot_common::types::tier::CapitalTier;
use std::collections::{HashMap, HashSet};

/// Observed buy event in the `alpha_window_ms` window after pool creation.
#[derive(Debug, Clone)]
pub struct WindowBuyEvent {
    pub buyer_wallet: String,
    pub token_amount: u64,
    pub slot: u64,
    pub in_creation_bundle: bool,
    /// Root funder wallet (<= 2 hops of first incoming SOL transfer).
    pub root_funder: Option<String>,
    /// Age of wallet's first transaction in minutes (`None` if unknown).
    pub wallet_age_minutes: Option<f64>,
}

/// Raw inputs for the Alpha Layer evaluation of a newly launched token.
#[derive(Debug, Clone, Default)]
pub struct AlphaSignalInputs {
    pub pool_creation_slot: u64,
    pub total_supply: u64,
    pub dev_wallet_tokens_bought: u64,
    pub top10_non_lp_tokens: u64,
    pub window_buys: Vec<WindowBuyEvent>,
    /// Developer history `(n_dumps, n_launches)`.
    pub dev_history: Option<(u32, u32)>,
}

/// Computed normalized metrics from `AlphaSignalInputs`.
#[derive(Debug, Clone)]
pub struct AlphaMetrics {
    pub dev_buy_pct: f64,
    pub bundler_pct: f64,
    pub funder_cluster_pct: f64,
    pub fresh_wallet_ratio: f64,
    /// `None` when unique buyers `N < min_buyers_for_concentration_check` (default 25).
    pub concentration_multiplier: Option<f64>,
    /// `None` when `n_launches == 0`.
    pub dev_history_score: Option<f64>,
}

/// Configuration for the Alpha Layer (§8.6 defaults).
#[derive(Debug, Clone)]
pub struct AlphaConfig {
    pub alpha_window_ms: u64,
    pub probe_window_sec: u64,
    pub min_buyers_for_concentration_check: usize,
    pub concentration_multiplier_max: f64,
    pub dev_history_block_threshold: f64,
    pub dev_history_caution_low: f64,
    pub label_cliff_window_sec: u64,
    pub label_dev_sell_pct: f64,
    pub label_drawdown_pct: f64,
    pub label_delay_min: u64,
    pub wallet_age_fresh_threshold_min: f64,
    pub weights: [f64; 5],
    pub q_probe_default: f64,
    pub delta_drift_estimate: f64,
    pub effective_loss_pct: f64,
    pub neutral_prior: f64,
    pub skip_probe_if_clean_and_below_p_star: bool,
}

impl Default for AlphaConfig {
    fn default() -> Self {
        Self {
            alpha_window_ms: 4000,
            probe_window_sec: 3,
            min_buyers_for_concentration_check: 25,
            concentration_multiplier_max: 3.0,
            dev_history_block_threshold: 0.50,
            dev_history_caution_low: 0.25,
            label_cliff_window_sec: 90,
            label_dev_sell_pct: 0.50,
            label_drawdown_pct: 0.80,
            label_delay_min: 60,
            wallet_age_fresh_threshold_min: 30.0,
            weights: [0.2, 0.2, 0.2, 0.2, 0.2],
            q_probe_default: 0.25,
            delta_drift_estimate: 0.05,
            effective_loss_pct: 0.65,
            neutral_prior: 0.35,
            skip_probe_if_clean_and_below_p_star: true,
        }
    }
}

impl AlphaConfig {
    /// Tier-specific hard max for `bundler_pct` (in `[0, 1]`).
    /// micro 70% / small 65% / medium 60% / large 55%
    pub fn bundler_hard_max(&self, tier: CapitalTier) -> f64 {
        match tier {
            CapitalTier::Micro => 0.70,
            CapitalTier::Small => 0.65,
            CapitalTier::Medium => 0.60,
            CapitalTier::Large => 0.55,
        }
    }

    /// Tier-specific hard max for `funder_cluster_pct` (in `[0, 1]`).
    /// micro 60% / small 55% / medium 50% / large 45%
    pub fn funder_cluster_hard_max(&self, tier: CapitalTier) -> f64 {
        match tier {
            CapitalTier::Micro => 0.60,
            CapitalTier::Small => 0.55,
            CapitalTier::Medium => 0.50,
            CapitalTier::Large => 0.45,
        }
    }

    /// Tier-specific floor for `size_multiplier`.
    /// micro/small 0.30, medium 0.25, large 0.20
    pub fn alpha_size_floor(&self, tier: CapitalTier) -> f64 {
        match tier {
            CapitalTier::Micro | CapitalTier::Small => 0.30,
            CapitalTier::Medium => 0.25,
            CapitalTier::Large => 0.20,
        }
    }
}

/// Probe vs Direct Full entry mode (§8.5).
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeMode {
    DirectFull,
    ProbeScale { q_probe: f64 },
}

/// Verdict of the Composite Alpha Gate (§8.3–§8.5).
#[derive(Debug, Clone, PartialEq)]
pub enum AlphaVerdict {
    Accept {
        alpha_risk_score: f64,
        size_multiplier: f64,
        probe_mode: ProbeMode,
        p_star: f64,
    },
    Reject {
        reason: String,
    },
}

/// Compute Wilson score 95% lower bound for developer dump probability (§2).
///
/// Returns `None` if `n_launches == 0`.
pub fn wilson_lower_dump_prob(n_dumps: u32, n_launches: u32) -> Option<f64> {
    if n_launches == 0 {
        return None;
    }
    let n = n_launches as f64;
    let k = n_dumps.min(n_launches) as f64;
    let p_hat = k / n;
    let z = 1.95996_f64;
    let z2 = z * z;

    let center = p_hat + z2 / (2.0 * n);
    let inner = (p_hat * (1.0 - p_hat) + z2 / (4.0 * n)) / n;
    let margin = z * inner.max(0.0).sqrt();
    let denom = 1.0 + z2 / n;

    Some(((center - margin) / denom).clamp(0.0, 1.0))
}

/// Label a completed launch as `"dump"` or `"not_dump"` (§2 Label rule).
pub fn classify_launch_label(
    dev_net_sell_pct_within_cliff: f64,
    max_drawdown_pct_within_cliff: f64,
    config: &AlphaConfig,
) -> &'static str {
    if dev_net_sell_pct_within_cliff >= config.label_dev_sell_pct
        || max_drawdown_pct_within_cliff >= config.label_drawdown_pct
    {
        "dump"
    } else {
        "not_dump"
    }
}

/// Compute on-chain signals and developer history score from `AlphaSignalInputs` (§1 & §2).
pub fn compute_alpha_metrics(inputs: &AlphaSignalInputs, config: &AlphaConfig) -> AlphaMetrics {
    let dev_buy_pct = if inputs.total_supply > 0 {
        (inputs.dev_wallet_tokens_bought as f64 / inputs.total_supply as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let total_buy_amount: u128 = inputs
        .window_buys
        .iter()
        .map(|b| b.token_amount as u128)
        .sum();

    let bundler_buy_amount: u128 = inputs
        .window_buys
        .iter()
        .filter(|b| b.in_creation_bundle || b.slot == inputs.pool_creation_slot)
        .map(|b| b.token_amount as u128)
        .sum();

    let bundler_pct = if total_buy_amount > 0 {
        (bundler_buy_amount as f64 / total_buy_amount as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Funder cluster percentage: max_cluster(sum buy_amount in cluster) / total_buy_amount
    let funder_cluster_pct = if total_buy_amount > 0 {
        let mut cluster_sums: HashMap<&str, u128> = HashMap::new();
        for buy in &inputs.window_buys {
            if let Some(ref funder) = buy.root_funder {
                *cluster_sums.entry(funder.as_str()).or_insert(0) += buy.token_amount as u128;
            }
        }
        let max_cluster = cluster_sums.values().copied().max().unwrap_or(0);
        (max_cluster as f64 / total_buy_amount as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Unique buyers and fresh wallet ratio
    let mut unique_buyers: HashSet<&str> = HashSet::new();
    let mut fresh_buyers: HashSet<&str> = HashSet::new();
    for buy in &inputs.window_buys {
        unique_buyers.insert(buy.buyer_wallet.as_str());
        if let Some(age_min) = buy.wallet_age_minutes {
            if age_min < config.wallet_age_fresh_threshold_min {
                fresh_buyers.insert(buy.buyer_wallet.as_str());
            }
        }
    }

    let n_buyers = unique_buyers.len();
    let fresh_wallet_ratio = if n_buyers > 0 {
        (fresh_buyers.len() as f64 / n_buyers as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Concentration multiplier (CM = top10_holder_pct * N / 10)
    let concentration_multiplier =
        if n_buyers >= config.min_buyers_for_concentration_check && inputs.total_supply > 0 {
            let top10_holder_pct =
                (inputs.top10_non_lp_tokens as f64 / inputs.total_supply as f64).clamp(0.0, 1.0);
            Some(top10_holder_pct * (n_buyers as f64) / 10.0)
        } else {
            None
        };

    let dev_history_score = inputs
        .dev_history
        .and_then(|(dumps, launches)| wilson_lower_dump_prob(dumps, launches));

    AlphaMetrics {
        dev_buy_pct,
        bundler_pct,
        funder_cluster_pct,
        fresh_wallet_ratio,
        concentration_multiplier,
        dev_history_score,
    }
}

/// Evaluate the Composite Alpha Gate, size derating, and Probe-Scale EV decision (§3 & §4).
pub fn evaluate_alpha_gate(
    metrics: &AlphaMetrics,
    tier: CapitalTier,
    config: &AlphaConfig,
) -> AlphaVerdict {
    // 1. Hard blocks
    if let Some(dev_score) = metrics.dev_history_score {
        if dev_score > config.dev_history_block_threshold {
            return AlphaVerdict::Reject {
                reason: format!(
                    "dev_history_score {:.3} > block_threshold {:.3}",
                    dev_score, config.dev_history_block_threshold
                ),
            };
        }
    }

    let bundler_max = config.bundler_hard_max(tier);
    if metrics.bundler_pct > bundler_max {
        return AlphaVerdict::Reject {
            reason: format!(
                "bundler_pct {:.3} > hard_max {:.3} for {:?}",
                metrics.bundler_pct, bundler_max, tier
            ),
        };
    }

    let cluster_max = config.funder_cluster_hard_max(tier);
    if metrics.funder_cluster_pct > cluster_max {
        return AlphaVerdict::Reject {
            reason: format!(
                "funder_cluster_pct {:.3} > hard_max {:.3} for {:?}",
                metrics.funder_cluster_pct, cluster_max, tier
            ),
        };
    }

    // 2. Soft score
    let norm_bundler = metrics.bundler_pct.clamp(0.0, 1.0);
    let norm_fresh = metrics.fresh_wallet_ratio.clamp(0.0, 1.0);
    let norm_cluster = metrics.funder_cluster_pct.clamp(0.0, 1.0);
    let norm_cm = metrics
        .concentration_multiplier
        .map(|cm| (cm / config.concentration_multiplier_max.max(1.0)).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let dev_term = metrics
        .dev_history_score
        .unwrap_or(config.neutral_prior)
        .clamp(0.0, 1.0);

    let [w1, w2, w3, w4, w5] = config.weights;
    let alpha_risk_score = (w1 * norm_bundler
        + w2 * norm_fresh
        + w3 * norm_cluster
        + w4 * norm_cm
        + w5 * dev_term)
        .clamp(0.0, 1.0);

    // 3. Size derate
    let floor = config.alpha_size_floor(tier);
    let size_multiplier = (1.0 - alpha_risk_score).clamp(floor, 1.0);

    // 4. Probe-scale EV breakeven
    let p_star = compute_breakeven_p_star(config.delta_drift_estimate, config.effective_loss_pct);
    let is_clean = alpha_risk_score <= config.dev_history_caution_low;
    let residual_p = metrics.dev_history_score.unwrap_or(alpha_risk_score);

    let probe_mode = if is_clean && residual_p < p_star && config.skip_probe_if_clean_and_below_p_star
    {
        ProbeMode::DirectFull
    } else {
        ProbeMode::ProbeScale {
            q_probe: config.q_probe_default,
        }
    };

    AlphaVerdict::Accept {
        alpha_risk_score,
        size_multiplier,
        probe_mode,
        p_star,
    }
}

/// Breakeven dump probability `p* = delta / (L + delta)` (§4).
#[inline]
pub fn compute_breakeven_p_star(delta_drift: f64, effective_loss_l: f64) -> f64 {
    let denom = effective_loss_l + delta_drift;
    if denom <= 0.0 {
        0.0
    } else {
        (delta_drift / denom).clamp(0.0, 1.0)
    }
}

/// Difference in EV: `EV_probe - EV_full = (1 - q) * S * [ p * L - (1 - p) * delta ]` (§4).
pub fn compute_probe_ev_advantage(
    target_size_sol: f64,
    q_probe: f64,
    residual_dump_prob_p: f64,
    delta_drift: f64,
    effective_loss_l: f64,
) -> f64 {
    let q = q_probe.clamp(0.0, 1.0);
    let p = residual_dump_prob_p.clamp(0.0, 1.0);
    (1.0 - q) * target_size_sol * (p * effective_loss_l - (1.0 - p) * delta_drift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wilson_lower_bound_known_values() {
        assert_eq!(wilson_lower_dump_prob(0, 0), None);
        // 0 dumps out of 10 launches -> lower bound ~0.0
        let p0 = wilson_lower_dump_prob(0, 10).unwrap();
        assert!(p0 < 1e-6);

        // 8 dumps out of 10 launches -> lower bound > 0.49
        let p8 = wilson_lower_dump_prob(8, 10).unwrap();
        assert!(p8 > 0.49 && p8 < 0.80, "got {p8}");
