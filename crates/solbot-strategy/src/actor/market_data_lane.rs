use solbot_common::types::MarketDataEvent;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct MarketDataLane {
    tx: mpsc::Sender<MarketDataEvent>,
    rx: mpsc::Receiver<MarketDataEvent>,
}

impl MarketDataLane {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(1);
        Self { tx, rx }
    }

    pub fn send(&mut self, event: MarketDataEvent) {
        if let Err(e) = self.tx.try_send(event) {
            if let Ok(dropped) = self.rx.try_recv() {
                tracing::debug!("Dropped oldest market data event: {:?}", dropped);
            }
            let _ = self.tx.try_send(e.into_inner());
        }
    }

    pub async fn recv(&mut self) -> Option<MarketDataEvent> {
        self.rx.recv().await
    }
}

impl Default for MarketDataLane {
    fn default() -> Self {
        Self::new()
    }
}
