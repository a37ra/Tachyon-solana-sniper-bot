use anyhow::Result;
use solbot_common::types::signal::SmartMoneyHit;
use tokio::sync::mpsc;

pub struct SmartMoneySubscriptionManager;

impl Default for SmartMoneySubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SmartMoneySubscriptionManager {
    pub fn new() -> Self {
        Self
    }

    pub async fn subscribe_wallets(&self, _wallets: Vec<String>) -> Result<mpsc::Receiver<SmartMoneyHit>> {
        let (_tx, rx) = mpsc::channel(100);
        tracing::info!("Smart Money wallet subscription manager initialized");
        Ok(rx)
    }
}
