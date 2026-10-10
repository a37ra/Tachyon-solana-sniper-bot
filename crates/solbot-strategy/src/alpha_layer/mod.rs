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
