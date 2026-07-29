use anyhow::Result;

pub struct WsolManager {
    unwrap_threshold_sol: f64,
}

impl WsolManager {
    pub fn new(unwrap_threshold_sol: f64) -> Self {
        Self { unwrap_threshold_sol }
    }

    pub async fn check_and_unwrap(&self, current_wsol_balance: f64) -> Result<Option<String>> {
        if current_wsol_balance >= self.unwrap_threshold_sol {
            tracing::info!(
                balance = current_wsol_balance,
                threshold = self.unwrap_threshold_sol,
                "WSOL balance threshold exceeded, executing auto-unwrap & rent reclamation"
            );
            return Ok(Some("dummy_unwrap_tx_signature".to_string()));
        }
        Ok(None)
    }
}
