pub mod control_lane;
pub mod market_data_lane;
pub mod mint_actor;

use solbot_common::types::{ControlEvent, MarketDataEvent};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct ActorMessage {
    pub control: Option<ControlEvent>,
    pub market_data: Option<MarketDataEvent>,
}

#[derive(Debug, Clone)]
pub struct ActorHandle {
    pub control_tx: mpsc::Sender<ControlEvent>,
    pub market_data_tx: mpsc::Sender<MarketDataEvent>,
}

#[derive(Debug, Clone)]
pub struct ActorConfig {
    pub control_queue_wait_deadline_ms: u64,
    pub control_event_processing_deadline_ms: u64,
    pub market_data_processing_deadline_ms: u64,
}

impl Default for ActorConfig {
    fn default() -> Self {
        Self {
            control_queue_wait_deadline_ms: 500,
            control_event_processing_deadline_ms: 10,
            market_data_processing_deadline_ms: 50,
        }
    }
}

use dashmap::DashMap;

#[derive(Debug, Clone)]
pub struct ActorRegistry {
    pub actors: Arc<DashMap<String, ActorHandle>>,
}

impl ActorRegistry {
    pub fn new() -> Self {
        Self {
            actors: Arc::new(DashMap::new()),
        }
    }

    pub fn register(&self, mint: String, handle: ActorHandle) {
        self.actors.insert(mint, handle);
    }

    pub fn get(&self, mint: &str) -> Option<ActorHandle> {
        self.actors.get(mint).map(|entry| entry.clone())
    }

    pub fn remove(&self, mint: &str) -> Option<ActorHandle> {
        self.actors.remove(mint).map(|(_, handle)| handle)
    }
}

impl Default for ActorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
