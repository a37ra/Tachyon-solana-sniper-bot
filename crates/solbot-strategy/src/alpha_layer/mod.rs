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
