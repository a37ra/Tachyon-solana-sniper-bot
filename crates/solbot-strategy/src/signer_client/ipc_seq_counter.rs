use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct IpcSeqCounter {
    counters: Arc<DashMap<String, Arc<AtomicU64>>>,
}

impl IpcSeqCounter {
    pub fn new() -> Self {
        Self {
            counters: Arc::new(DashMap::new()),
        }
    }

    pub fn next_seq(&self, context_key: &str) -> u64 {
        let counter = self.counters
            .entry(context_key.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)));
        counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn set_seq(&self, context_key: &str, last_seq: u64) {
        let counter = self.counters
            .entry(context_key.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)));
        counter.store(last_seq, Ordering::SeqCst);
    }
}

impl Default for IpcSeqCounter {
    fn default() -> Self {
        Self::new()
    }
}
