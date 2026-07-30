use super::SubmitResult;
use anyhow::Result;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

#[derive(Debug, Clone)]
pub struct JitoRegionPingResult {
    pub name: String,
    pub endpoint: String,
    pub grpc_endpoint: String,
    pub rtt_ms: f64,
    pub success: bool,
    pub error: Option<String>,
}

pub struct JitoClient {
    endpoint: String,
    timeout_ms: u64,
}

impl JitoClient {
    pub fn new(endpoint: String, timeout_ms: u64) -> Self {
        Self { endpoint, timeout_ms }
    }

    /// Converts a standard URL (e.g. `https://frankfurt.mainnet.block-engine.jito.wtf`)
    /// into a valid gRPC endpoint string (e.g. `frankfurt.mainnet.block-engine.jito.wtf:443`).
    pub fn to_grpc_endpoint(url: &str) -> String {
        let stripped = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');

        if stripped.contains(':') {
            stripped.to_string()
        } else {
            format!("{}:443", stripped)
        }
    }

    /// Measures network TCP/TLS RTT ping to Jito Block Engine endpoints.
    pub async fn ping_region(name: &str, endpoint: &str) -> JitoRegionPingResult {
        let grpc_endpoint = Self::to_grpc_endpoint(endpoint);
        let start = Instant::now();

        // Extract host and port for TCP ping
        let host_port = grpc_endpoint.clone();
        let ping_future = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&host_port));

        match ping_future.await {
            Ok(Ok(_stream)) => {
                let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;
                JitoRegionPingResult {
                    name: name.to_string(),
                    endpoint: endpoint.to_string(),
                    grpc_endpoint,
                    rtt_ms,
                    success: true,
                    error: None,
                }
            }
            Ok(Err(e)) => JitoRegionPingResult {
                name: name.to_string(),
                endpoint: endpoint.to_string(),
                grpc_endpoint,
                rtt_ms: 9999.0,
                success: false,
                error: Some(e.to_string()),
            },
            Err(_) => JitoRegionPingResult {
                name: name.to_string(),
                endpoint: endpoint.to_string(),
                grpc_endpoint,
                rtt_ms: 9999.0,
                success: false,
                error: Some("Timeout (3s)".to_string()),
            },
        }
    }

    /// Measure RTT across all configured Jito regions and return sorted by lowest latency.
    pub async fn ping_all_regions(regions: &[(&str, &str)]) -> Vec<JitoRegionPingResult> {
        let mut tasks = Vec::new();
        for &(name, endpoint) in regions {
            let name_owned = name.to_string();
            let endpoint_owned = endpoint.to_string();
            tasks.push(tokio::spawn(async move {
                Self::ping_region(&name_owned, &endpoint_owned).await
            }));
        }

        let mut results = Vec::new();
        for task in tasks {
            if let Ok(res) = task.await {
                results.push(res);
            }
        }

        results.sort_by(|a, b| a.rtt_ms.partial_cmp(&b.rtt_ms).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    pub async fn submit_bundle(&self, signed_tx_base64: &str, tip_lamports: u64) -> Result<SubmitResult> {
        tracing::info!(
            endpoint = %self.endpoint,
            grpc_endpoint = %Self::to_grpc_endpoint(&self.endpoint),
            tip = tip_lamports,
            "Submitting Jito bundle"
        );

        // Simulated bundle submission with realistic Jito latency
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok(SubmitResult::Confirmed {
            signature: format!("jito_sig_{}", &signed_tx_base64[..std::cmp::min(10, signed_tx_base64.len())]),
            slot: 250000000,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_grpc_endpoint() {
        assert_eq!(
            JitoClient::to_grpc_endpoint("https://frankfurt.mainnet.block-engine.jito.wtf"),
            "frankfurt.mainnet.block-engine.jito.wtf:443"
        );
        assert_eq!(
            JitoClient::to_grpc_endpoint("frankfurt.mainnet.block-engine.jito.wtf:443"),
            "frankfurt.mainnet.block-engine.jito.wtf:443"
        );
    }
}
