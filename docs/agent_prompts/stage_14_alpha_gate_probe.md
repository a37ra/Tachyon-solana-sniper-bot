# Agent Prompt — Этап 14: Composite Gate + Probe-Scale Entry

## Context

Stages 12–13 produce features + dev history. This stage **decides and acts**:

1. Hard blocks → reject (not honeypot; bucket `alpha_hard_block`)
2. Soft score → size_multiplier inside Kelly clamp
3. Probe-scale entry vs direct_full
4. Probe dump → **4th trigger** into `emergency_exit_escalation` (TZ 2.3)

**Read:** TZ §8.4–8.6, formulas §3–4, `config/alpha_layer.yaml`, existing `risk_gate/mod.rs`, `exit/escalation.rs`, `sizing/*`, `actor/mint_actor.rs`.

## Architecture rules (non-negotiable)

```
signal → [alpha Layer 0] → [risk_gate 4.2] → sizing → build → sign → route
```

- Alpha **never** weakens freeze/LP/Token2022/simulate_sell.
- Separate latency span: `alpha_gate_latency_ms` ≠ `risk_gate_latency_ms`.
- `alpha_window_ms` may wait for features; do not fold wait into risk_gate hard_timeout semantics without documenting.

## Hard blocks (any → reject)

```
dev_history_score > block_threshold[tier]  # only if Some
bundler_pct > hard_max_pct[tier]
funder_cluster_pct > hard_max_pct[tier]
```

Log loudly; persist `alpha_signals.decision = hard_block`.

## Soft score

```
alpha_risk_score = Σ wi*norm(fi) + w5*(dev_history_score ?? neutral_prior)
size_multiplier = clamp(1 - alpha_risk_score, alpha_size_floor[tier], 1.0)
final_size = adaptive_size * size_multiplier  # still ≤ size_max_pct
```

Default wi=0.2 until stage 15.

## Probe-scale

```
p* = δ / (L + δ)   # δ from config delta_drift_estimate_pct; L from tier effective_loss
IF clean AND estimated_p < p* AND skip_probe_if_clean → direct_full
ELSE → buy q*S (probe); after probe_window_sec:
   if dump signals → emergency_exit (new trigger variant)
   else confirm buy (1-q)*S same position_id, VWAP entry_price
```

Dump detection during probe: reuse simplified label rule on short window and/or rapid dev sell / drawdown — document chosen heuristic; must call **same** escalation module as hard SL.

## Deliverables

- [ ] `composite_gate.rs`
- [ ] `probe_scale.rs`
- [ ] Wire into mint_actor / main buy path
- [ ] Config load from `alpha_layer.yaml`
- [ ] pipeline_stats bucket `alpha_hard_block`
- [ ] E2E paper tests: hard_block rejects; probe→confirm; probe→exit
- [ ] Plan checkboxes

## Constraints

- Fail-closed if alpha enabled but features timed out? Prefer **configurable**: default reject or pass-through with max derate — **choose reject for live-ready**, pass-through only if `alpha_layer.enabled=false`.
- No hardcoded fee_recipient (already Global-loaded).
- Paper mode must exercise probe path without real funds.

## Verification

```bash
cargo test -p solbot-strategy --lib alpha composite probe
timeout 120 cargo run -p solbot-strategy --release -- --paper
# expect: some rejects bucket alpha_hard_block; ACCEPTs may drop vs pre-alpha (good)
```

## Anti-patterns

- Putting Anchor 3005 back into BenignDummyWallet
- soft_score bypassing hard_blocks
- New position_id on confirm buy
- Waiting full 60s label_delay on hot path
- Claiming calibration done (that's stage 15)
