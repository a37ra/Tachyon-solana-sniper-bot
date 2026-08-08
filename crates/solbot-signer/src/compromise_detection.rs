use std::collections::VecDeque;
use std::sync::RwLock;
use std::time::{Duration, Instant};

pub struct CompromiseDetection {
    window: Duration,
    threshold: u32,
    timestamps: RwLock<VecDeque<Instant>>,
}

impl CompromiseDetection {
    pub fn new(window_secs: u64, threshold: u32) -> Self {
        Self {
            window: Duration::from_secs(window_secs),
            threshold,
            timestamps: RwLock::new(VecDeque::new()),
        }
    }

    pub fn check_signing_rate(&self) -> bool {
        let mut q = self.timestamps.write().unwrap();
        let now = Instant::now();

        // Remove old timestamps
        while let Some(&t) = q.front() {
            if now.duration_since(t) > self.window {
                q.pop_front();
            } else {
                break;
            }
        }

        // Check threshold
        if q.len() >= self.threshold as usize {
            return false; // Anomalous rate detected
        }

        q.push_back(now);
        true
    }
}
