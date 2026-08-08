//! DEX live readiness tracking.
//!
//! This module defines the readiness state for each DEX, allowing
//! Live mode to be enabled only after real pricing has been verified.

use serde::{Deserialize, Serialize};

/// Live readiness status for a DEX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DexLiveReadiness {
    /// Real pricing has been verified - Live mode is safe.
    Ready,
    /// Approximate pricing (e.g., single-bin DLMM) - may be inaccurate for large orders.
    Approximate,
    /// Only placeholder pricing available - Live mode will error.
    PlaceholderOnly,
}

impl DexLiveReadiness {
    /// Check if Live mode is allowed for this DEX.
    pub fn is_live_allowed(&self) -> bool {
        matches!(
            self,
            DexLiveReadiness::Ready | DexLiveReadiness::Approximate
        )
    }
}

impl Default for DexLiveReadiness {
    fn default() -> Self {
        DexLiveReadiness::PlaceholderOnly
    }
}
