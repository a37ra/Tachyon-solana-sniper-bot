# Agent Prompt — Этап 8: Observability & Alerting

## Context

Wire metrics/traces/alerts. Templates/YAML exist; HTTP clients incomplete. TZ §1.3, §7.1–7.2.

## Goal

1. OTel spans: `signal_received`, `risk_gate_start/end`, `alpha_gate_*` (stub ok if alpha not built), `tx_built`, `signed`, `bundle_submitted`
2. Histograms: `risk_gate_latency_ms`, `submission_engine_latency_ms`, `end_to_end_latency_ms`
3. Telegram HTTP send + rate limit (MarkdownV2 already formatted)
4. Email SMTP fallback for critical alerts
5. Prometheus `/metrics` endpoint (or OTel prometheus exporter)

## Constraints

- Never log private keys, full signed tx blobs, or seed phrases.
- Alerts must include mint, reason, signatures when available.
- Do not block hot path >1ms on alert send (async queue).

## Deliverables

- [ ] Spans wired on buy path in strategy main/actor
- [ ] Telegram client + rate limiter
- [ ] Metrics export
- [ ] Docs snippet in `docs/RUNBOOK.md` how to scrape metrics
- [ ] Plan checkboxes

## Verification

```bash
cargo test -p solbot-notify -p solbot-telegram
# optional: run paper, confirm spans in collector if configured
```

## Hints

- Grep `otel`, `tracing::info_span`, `BotEvent`.
- Keep alpha span name reserved: `alpha_gate_latency_ms` even if no-op until stage 14.
