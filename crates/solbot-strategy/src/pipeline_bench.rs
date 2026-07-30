//! Pipeline benchmarking and latency metrics tracker for Paper Trading / Dry-Run mode.

use std::time::Instant;

/// Benchmark metrics recorded across the pipeline phases for a detected pool signal.
#[derive(Debug, Clone, Default)]
pub struct PipelineMetrics {
    pub mint: String,
    pub pool_address: String,
    pub dex: String,
    pub parse_decode_us: u64,
    pub anti_rug_us: u64,
    pub tx_build_sign_us: u64,
    pub rpc_sim_rtt_ms: f64,
    pub inbound_stream_latency_ms: f64,
}

impl PipelineMetrics {
    pub fn new(mint: impl Into<String>, pool_address: impl Into<String>, dex: impl Into<String>) -> Self {
        Self {
            mint: mint.into(),
            pool_address: pool_address.into(),
            dex: dex.into(),
            ..Default::default()
        }
    }

    pub fn total_code_latency_ms(&self) -> f64 {
        (self.parse_decode_us + self.anti_rug_us + self.tx_build_sign_us) as f64 / 1000.0
    }

    /// Formats and outputs the exact tree log structure requested by the user.
    pub fn print_tree_log(&self) {
        let short_mint = if self.mint.len() > 10 {
            format!("{}...{}", &self.mint[..4], &self.mint[self.mint.len() - 4..])
        } else {
            self.mint.clone()
        };

        let parse_ms = self.parse_decode_us as f64 / 1000.0;
        let rug_ms = self.anti_rug_us as f64 / 1000.0;
        let tx_ms = self.tx_build_sign_us as f64 / 1000.0;
        let total_ms = self.total_code_latency_ms();

        println!("\n[NEW POOL DETECTED] Mint: {}", short_mint);
        println!("├── 1. Parsing & Decode:   {:>6.2} ms", parse_ms);
        println!("├── 2. Anti-Rug Filters:  {:>6.2} ms", rug_ms);
        println!("├── 3. Tx Build & Sign:   {:>6.2} ms", tx_ms);
        println!("├── TOTAL CODE LATENCY:   {:>6.2} ms", total_ms);
        println!("└── RPC Simulation RTT:  {:>6.2} ms", self.rpc_sim_rtt_ms);
        if self.inbound_stream_latency_ms > 0.0 {
            println!("    (Inbound Stream Age: {:>6.2} ms)", self.inbound_stream_latency_ms);
        }
    }
}

/// Helper timer to measure microsecond latency of individual execution sections.
pub struct PhaseTimer {
    start: Instant,
}

impl PhaseTimer {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
}
