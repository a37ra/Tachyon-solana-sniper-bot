//! Process-wide signal pipeline counters (shared across all listener workers).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static ACCEPT: AtomicU64 = AtomicU64::new(0);
static REJECT: AtomicU64 = AtomicU64::new(0);
static DROP: AtomicU64 = AtomicU64::new(0);
static QUEUED: AtomicU64 = AtomicU64::new(0);

fn reject_map() -> &'static Mutex<HashMap<String, u64>> {
    static M: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn drop_map() -> &'static Mutex<HashMap<String, u64>> {
    static M: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn stale_ages() -> &'static Mutex<Vec<u64>> {
    static M: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn inc_queued() {
    QUEUED.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_accept() {
    ACCEPT.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_reject(bucket: &str) {
    REJECT.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut m) = reject_map().lock() {
        *m.entry(bucket.to_string()).or_insert(0) += 1;
    }
}

pub fn inc_drop(bucket: &str) {
    DROP.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut m) = drop_map().lock() {
        *m.entry(bucket.to_string()).or_insert(0) += 1;
    }
}

pub fn record_stale_age_ms(age_ms: u64) {
    if let Ok(mut v) = stale_ages().lock() {
        if v.len() < 10_000 {
            v.push(age_ms);
        }
    }
}

pub fn snapshot() -> PipelineStatsSnapshot {
    let reject = reject_map().lock().map(|m| m.clone()).unwrap_or_default();
    let dropped = drop_map().lock().map(|m| m.clone()).unwrap_or_default();
    let ages = stale_ages().lock().map(|v| v.clone()).unwrap_or_default();
    let (stale_min, stale_p50, stale_p90, stale_max) = percentile_ages(&ages);
    PipelineStatsSnapshot {
        accept: ACCEPT.load(Ordering::Relaxed),
        reject: REJECT.load(Ordering::Relaxed),
        dropped: DROP.load(Ordering::Relaxed),
        queued: QUEUED.load(Ordering::Relaxed),
        reject_by_reason: reject,
        drop_by_reason: dropped,
        stale_age_min_ms: stale_min,
        stale_age_p50_ms: stale_p50,
        stale_age_p90_ms: stale_p90,
        stale_age_max_ms: stale_max,
        stale_age_n: ages.len() as u64,
    }
}

fn percentile_ages(ages: &[u64]) -> (u64, u64, u64, u64) {
    if ages.is_empty() {
        return (0, 0, 0, 0);
    }
    let mut a = ages.to_vec();
    a.sort_unstable();
    let n = a.len();
    (
        a[0],
        a[n / 2],
        a[(n * 9 / 10).min(n - 1)],
        a[n - 1],
    )
}

#[derive(Debug, Clone)]
pub struct PipelineStatsSnapshot {
    pub accept: u64,
    pub reject: u64,
    pub dropped: u64,
    pub queued: u64,
    pub reject_by_reason: HashMap<String, u64>,
    pub drop_by_reason: HashMap<String, u64>,
    pub stale_age_min_ms: u64,
    pub stale_age_p50_ms: u64,
    pub stale_age_p90_ms: u64,
    pub stale_age_max_ms: u64,
    pub stale_age_n: u64,
}

/// Spawn a single global stats logger (call once from main).
pub fn spawn_stats_logger() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let s = snapshot();
            tracing::info!(
                accept = s.accept,
                reject = s.reject,
                dropped = s.dropped,
                queued = s.queued,
                reject_by_reason = ?s.reject_by_reason,
                drop_by_reason = ?s.drop_by_reason,
                stale_age_n = s.stale_age_n,
                stale_age_min_ms = s.stale_age_min_ms,
                stale_age_p50_ms = s.stale_age_p50_ms,
                stale_age_p90_ms = s.stale_age_p90_ms,
                stale_age_max_ms = s.stale_age_max_ms,
                "PIPELINE STATS (global cumulative)"
            );
        }
    });
}
