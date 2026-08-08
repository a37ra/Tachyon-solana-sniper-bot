use solbot_common::types::ControlEvent;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

pub static CONTROL_LANE_SEND_TIMEOUT_TOTAL: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct ControlLane {
    pub tx: mpsc::Sender<ControlEvent>,
    pub rx: mpsc::Receiver<ControlEvent>,
}

impl ControlLane {
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        Self { tx, rx }
    }

    pub async fn send(
        &self,
        event: ControlEvent,
        timeout_ms: u64,
    ) -> Result<(), mpsc::error::SendError<ControlEvent>> {
        let timeout = Duration::from_millis(timeout_ms);

        match tokio::time::timeout(timeout, self.tx.send(event.clone())).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                CONTROL_LANE_SEND_TIMEOUT_TOTAL.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("Control lane send timeout after {}ms", timeout_ms);
                Err(mpsc::error::SendError(event))
            }
        }
    }

    pub fn try_send(
        &self,
        event: ControlEvent,
    ) -> Result<(), mpsc::error::TrySendError<ControlEvent>> {
        self.tx.try_send(event)
    }
}

impl Default for ControlLane {
    fn default() -> Self {
        Self::new(64)
    }
}
