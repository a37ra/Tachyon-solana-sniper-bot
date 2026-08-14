//! Observability & Prometheus Metrics Exporter (TZ §1.3, §7.1).
//!
//! Exposes OTel tracing spans & Prometheus `/metrics` endpoint on port 9090.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Global Prometheus metrics container.
#[derive(Debug, Default)]
pub struct MetricsRegistry {
    pub signals_received_total: AtomicU64,
    pub risk_gate_accept_total: AtomicU64,
    pub risk_gate_reject_total: AtomicU64,
    pub tx_built_total: AtomicU64,
    pub tx_signed_total: AtomicU64,
    pub bundles_submitted_total: AtomicU64,

    // Latency accumulators (micros) and counts
    pub risk_gate_latency_us_total: AtomicU64,
    pub risk_gate_latency_count: AtomicU64,

    pub submission_engine_latency_us_total: AtomicU64,
    pub submission_engine_latency_count: AtomicU64,

    pub end_to_end_latency_us_total: AtomicU64,
    pub end_to_end_latency_count: AtomicU64,

    pub alpha_gate_latency_us_total: AtomicU64,
    pub alpha_gate_latency_count: AtomicU64,
}

impl MetricsRegistry {
    pub fn global() -> &'static MetricsRegistry {
        static INSTANCE: std::sync::OnceLock<MetricsRegistry> = std::sync::OnceLock::new();
        INSTANCE.get_or_init(MetricsRegistry::default)
    }

    pub fn record_signal_received(&self) {
        self.signals_received_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_risk_gate(&self, accepted: bool, latency_ms: f64) {
        if accepted {
            self.risk_gate_accept_total.fetch_add(1, Ordering::Relaxed);
        } else {
            self.risk_gate_reject_total.fetch_add(1, Ordering::Relaxed);
        }
        let us = (latency_ms * 1000.0) as u64;
        self.risk_gate_latency_us_total.fetch_add(us, Ordering::Relaxed);
        self.risk_gate_latency_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tx_built(&self) {
        self.tx_built_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_signed(&self) {
        self.tx_signed_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_bundle_submitted(&self, latency_ms: f64) {
        self.bundles_submitted_total.fetch_add(1, Ordering::Relaxed);
        let us = (latency_ms * 1000.0) as u64;
        self.submission_engine_latency_us_total.fetch_add(us, Ordering::Relaxed);
        self.submission_engine_latency_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_end_to_end(&self, latency_ms: f64) {
        let us = (latency_ms * 1000.0) as u64;
        self.end_to_end_latency_us_total.fetch_add(us, Ordering::Relaxed);
        self.end_to_end_latency_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_alpha_gate(&self, latency_ms: f64) {
        let us = (latency_ms * 1000.0) as u64;
        self.alpha_gate_latency_us_total.fetch_add(us, Ordering::Relaxed);
        self.alpha_gate_latency_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn render_prometheus(&self) -> String {
        let risk_avg_ms = avg_ms(
            self.risk_gate_latency_us_total.load(Ordering::Relaxed),
            self.risk_gate_latency_count.load(Ordering::Relaxed),
        );
        let sub_avg_ms = avg_ms(
            self.submission_engine_latency_us_total.load(Ordering::Relaxed),
            self.submission_engine_latency_count.load(Ordering::Relaxed),
        );
        let e2e_avg_ms = avg_ms(
            self.end_to_end_latency_us_total.load(Ordering::Relaxed),
            self.end_to_end_latency_count.load(Ordering::Relaxed),
        );
        let alpha_avg_ms = avg_ms(
            self.alpha_gate_latency_us_total.load(Ordering::Relaxed),
            self.alpha_gate_latency_count.load(Ordering::Relaxed),
        );

        format!(
            "# HELP solbot_signals_received_total Total pool signals received\n\
             # TYPE solbot_signals_received_total counter\n\
             solbot_signals_received_total {}\n\n\
             # HELP solbot_risk_gate_accept_total Signals passed risk gate\n\
             # TYPE solbot_risk_gate_accept_total counter\n\
             solbot_risk_gate_accept_total {}\n\n\
             # HELP solbot_risk_gate_reject_total Signals rejected by risk gate\n\
             # TYPE solbot_risk_gate_reject_total counter\n\
             solbot_risk_gate_reject_total {}\n\n\
             # HELP solbot_tx_built_total Total transactions built\n\
             # TYPE solbot_tx_built_total counter\n\
             solbot_tx_built_total {}\n\n\
             # HELP solbot_tx_signed_total Total transactions signed by Signer IPC\n\
             # TYPE solbot_tx_signed_total counter\n\
             solbot_tx_signed_total {}\n\n\
             # HELP solbot_bundles_submitted_total Total bundles/txs submitted via Router\n\
             # TYPE solbot_bundles_submitted_total counter\n\
             solbot_bundles_submitted_total {}\n\n\
             # HELP solbot_risk_gate_latency_ms Average risk gate execution latency in ms\n\
             # TYPE solbot_risk_gate_latency_ms gauge\n\
             solbot_risk_gate_latency_ms {:.3}\n\n\
             # HELP solbot_submission_engine_latency_ms Average submission engine latency in ms\n\
             # TYPE solbot_submission_engine_latency_ms gauge\n\
             solbot_submission_engine_latency_ms {:.3}\n\n\
             # HELP solbot_end_to_end_latency_ms Average end-to-end signal to submit latency in ms\n\
             # TYPE solbot_end_to_end_latency_ms gauge\n\
             solbot_end_to_end_latency_ms {:.3}\n\n\
             # HELP solbot_alpha_gate_latency_ms Reserved Alpha Layer gate latency in ms\n\
             # TYPE solbot_alpha_gate_latency_ms gauge\n\
             solbot_alpha_gate_latency_ms {:.3}\n",
            self.signals_received_total.load(Ordering::Relaxed),
            self.risk_gate_accept_total.load(Ordering::Relaxed),
            self.risk_gate_reject_total.load(Ordering::Relaxed),
            self.tx_built_total.load(Ordering::Relaxed),
            self.tx_signed_total.load(Ordering::Relaxed),
            self.bundles_submitted_total.load(Ordering::Relaxed),
            risk_avg_ms,
            sub_avg_ms,
            e2e_avg_ms,
            alpha_avg_ms,
        )
    }
}

fn avg_ms(total_us: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        (total_us as f64 / count as f64) / 1000.0
    }
}

/// Spawns lightweight HTTP server for Prometheus `/metrics` scraping.
pub fn spawn_metrics_server(port: u16) {
    tokio::spawn(async move {
        let addr = format!("0.0.0.0:{}", port);
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(port, error = %e, "Failed to bind Prometheus metrics HTTP server");
                return;
            }
        };

        tracing::info!(addr = %addr, "📊 Prometheus metrics endpoint running at http://{}/metrics", addr);

        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                continue;
            };

            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;

                let body = MetricsRegistry::global().render_prometheus();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );

                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_registry_records_and_renders() {
        let reg = MetricsRegistry::default();
        reg.record_signal_received();
        reg.record_risk_gate(true, 15.5);
        reg.record_tx_built();
        reg.record_signed();
        reg.record_bundle_submitted(45.0);
        reg.record_end_to_end(60.5);
        reg.record_alpha_gate(0.5);

        let rendered = reg.render_prometheus();
        assert!(rendered.contains("solbot_signals_received_total 1"));
        assert!(rendered.contains("solbot_risk_gate_accept_total 1"));
        assert!(rendered.contains("solbot_tx_built_total 1"));
        assert!(rendered.contains("solbot_tx_signed_total 1"));
        assert!(rendered.contains("solbot_bundles_submitted_total 1"));
        assert!(rendered.contains("solbot_alpha_gate_latency_ms"));
    }
}
