use super::events::BotEvent;
use tokio::sync::broadcast;

pub struct EventBus {
    sender: broadcast::Sender<BotEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn publish(&self, event: BotEvent) {
        if let Err(e) = self.sender.send(event) {
            tracing::warn!("Failed to publish BotEvent (no subscribers): {}", e);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BotEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(100)
    }
}
