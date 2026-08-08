use dashmap::DashMap;
use solbot_common::types::{Signal, SmartMoneyHit};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct DedupFilter {
    seen: DashMap<String, Instant>,
    ttl: Duration,
}

impl DedupFilter {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            seen: DashMap::new(),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    pub fn check_signal(&self, signal: &Signal) -> bool {
        let key = self.dex_signal_key(signal);
        self.check_and_insert(key)
    }

    pub fn check_smart_money_hit(&self, hit: &SmartMoneyHit) -> bool {
        let key = self.smart_money_key(hit);
        self.check_and_insert(key)
    }

    fn dex_signal_key(&self, signal: &Signal) -> String {
        format!("{}:{}", signal.mint, signal.pool_address)
    }

    fn smart_money_key(&self, hit: &SmartMoneyHit) -> String {
        format!("{}:{}:{}", hit.wallet_address, hit.mint, hit.tx_signature)
    }

    fn check_and_insert(&self, key: String) -> bool {
        let now = Instant::now();

        if let Some(entry) = self.seen.get(&key) {
            if now.duration_since(*entry) < self.ttl {
                return false;
            }
        }

        self.seen.insert(key, now);
        true
    }

    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        self.seen
            .retain(|_, instant| now.duration_since(*instant) < self.ttl);
    }
}

impl Default for DedupFilter {
    fn default() -> Self {
        Self::new(30)
    }
}
