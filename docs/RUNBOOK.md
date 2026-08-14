# Solbot Operations Runbook

## Observability & Monitoring (TZ §7.1)

### Prometheus Metrics Endpoint

The bot exposes a Prometheus-compatible `/metrics` endpoint on port **9090** (configurable via `METRICS_PORT`).

#### Available Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `solbot_signals_received_total` | Counter | Total pool signals entering the buy pipeline |
| `solbot_risk_gate_accept_total` | Counter | Signals that passed all risk gate checks |
| `solbot_risk_gate_reject_total` | Counter | Signals rejected by risk gate |
| `solbot_tx_built_total` | Counter | Total swap transactions built |
| `solbot_tx_signed_total` | Counter | Transactions signed by Signer IPC |
| `solbot_bundles_submitted_total` | Counter | Bundles/txs submitted via Router |
| `solbot_risk_gate_latency_ms` | Gauge (avg) | Average risk gate execution latency |
| `solbot_submission_engine_latency_ms` | Gauge (avg) | Average Router submission latency |
| `solbot_end_to_end_latency_ms` | Gauge (avg) | Average signal-to-confirm latency |
| `solbot_alpha_gate_latency_ms` | Gauge (avg) | Reserved Alpha Layer gate latency |

#### Quick Test

```bash
# Verify the metrics endpoint is running
curl http://localhost:9090/metrics
```

#### Prometheus `prometheus.yml` Scrape Config

```yaml
scrape_configs:
  - job_name: 'solbot'
    scrape_interval: 15s
    static_configs:
      - targets: ['localhost:9090']
    metrics_path: /metrics
```

#### Grafana Dashboard (Optional)

Import the Prometheus data source and create panels for:
- **Signal throughput**: `rate(solbot_signals_received_total[5m])`
- **Risk gate accept rate**: `solbot_risk_gate_accept_total / solbot_signals_received_total`
- **End-to-end latency**: `solbot_end_to_end_latency_ms`
- **Bundle submission rate**: `rate(solbot_bundles_submitted_total[5m])`

---

### OTel Tracing Spans

The following spans are instrumented on the hot path:

| Span Name | Location | Description |
|-----------|----------|-------------|
| `signal_received` | `main.rs` signal loop | Wraps entire signal processing |
| `risk_gate` | `execute_buy_pipeline` | Risk gate check duration |
| `tx_built` | `execute_buy_pipeline` | Transaction construction |
| `signed` | `execute_buy_pipeline` | Signer IPC round-trip (logged on success) |
| `bundle_submitted` | `execute_buy_pipeline` | Router submission + confirmation |
| `execute_buy_pipeline` | `mint_actor.rs` | Full buy pipeline end-to-end |
| `alpha_gate_latency_ms` | Reserved | Alpha Layer gate (Stage 14) |

To export spans to an OTel Collector, configure:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4317"
export OTEL_SERVICE_NAME="solbot-strategy"
```

---

## Alerting (TZ §7.2)

### Telegram Alerts

Configure via environment variables:

```bash
export TELEGRAM_BOT_TOKEN="123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"
export TELEGRAM_CHAT_ID="-1001234567890"
```

**Rate limits**: The bot enforces Telegram API limits (max 1 msg/sec per chat, 20 msg/sec global).

**Alert events**:
- `PositionOpened` — new position entered
- `PositionClosed` — position exited with PnL summary
- `CircuitBreakerTriggered` — circuit breaker activated (CRITICAL)
- `StuckPosition` — position stuck, manual intervention needed (CRITICAL)
- `KillSwitchActivated` — emergency kill switch (CRITICAL)
- `SigningRateAnomaly` — suspicious signing rate detected (CRITICAL)

### Email SMTP Fallback

For **critical alerts only** (`CircuitBreakerTriggered`, `StuckPosition`, `KillSwitchActivated`, `SigningRateAnomaly`), the system falls back to SMTP email when Telegram API is unavailable.

```bash
export SMTP_HOST="smtp.gmail.com"
export SMTP_PORT="587"
export SMTP_USER="bot-alerts@example.com"
export SMTP_PASS="app-password"
export ALERT_RECIPIENT_EMAIL="admin@example.com"
```

---

## Running the Bot

### Paper Trading (Dry-Run)

```bash
cargo run -p solbot-strategy --bin solbot-strategy --release -- --paper
```

### Live Trading

```bash
# Ensure signer process is running first
cargo run -p solbot-signer --release &

# Start strategy
cargo run -p solbot-strategy --bin solbot-strategy --release
```

### Running Tests

```bash
# Strategy unit tests (237+ tests)
cargo test -p solbot-strategy --lib

# Notification & Telegram tests
cargo test -p solbot-notify -p solbot-telegram
```

---

## Security Constraints

- **Never** log private keys, full signed tx blobs, or seed phrases.
- Alerts include: mint address, reason, transaction signatures (when available).
- Alert dispatch is **async** — never blocks hot path > 1ms.
