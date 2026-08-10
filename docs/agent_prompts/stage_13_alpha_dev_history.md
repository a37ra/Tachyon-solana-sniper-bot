# Agent Prompt — Этап 13: Dev History (Wilson score + labeling worker)

## Context

Stage 12 should provide feature collection. This stage adds **creator reputation**:

- Retrospective labels dump/not_dump (DB→SB→DS window 90s)
- Wilson 95% lower bound on dump rate
- SQLite `dev_wallet_history` / `dev_launch_labels`

**Read:** TZ §8.3, `docs/alpha/ALPHA_LAYER_FORMULAS.md` §2, migration `0004_alpha_layer.sql`, stage 12 code.

## Goal

1. `alpha/dev_history.rs`
   - `wilson_lower_dump_prob(k, n)` exact formula with z=1.95996
   - `n==0 → score=None`; else Some(wilson)
2. `alpha/labeling_worker.rs`
   - Background task: for pending launches older than `label_delay_min`, compute:
     - `dev_net_sell_pct` in first `label_cliff_window_sec` (90s)
     - `max_drawdown_pct` from local ATH in that window
   - Label dump if sell≥50% OR dd≥80%; else not_dump
   - Update `n_launches`, `n_dumps`, wilson, timestamps
3. Creator extraction from create path:
   - **Pump:** bonding curve account `creator` at byte offset 49 (already parsed in `tx_builder/pump_fun.rs`)
   - Other DEX: best-effort from create tx fee payer / known creator account meta — document limitations
4. `state/dev_wallet_history_repo.rs`
5. Tests: synthetic n=1..40, wilson moves toward true p; single dump does not force score≈1.0 at n=1

## Constraints

- Labeling is **offline/async** — must not add seconds to buy hot path. Hot path only **reads** cached wilson.
- Do not hard_block in this stage (wire read API for stage 14).
- Deterministic pure functions for wilson + label decision (unit-test without RPC).

## Wilson (copy exactly)

```
p_hat = k/n
z = 1.95996
center = p_hat + z^2/(2n)
margin = z * sqrt( (p_hat*(1-p_hat) + z^2/(4n)) / n )
denom = 1 + z^2/n
wilson_lower = (center - margin) / denom
```

## Verification

```bash
cargo test -p solbot-strategy --lib dev_history labeling wilson
```

## Done when

- [ ] Worker can label synthetic launches
- [ ] Repo round-trip
- [ ] Plan stage 13 checkboxes
- [ ] notes.md: how worker is spawned in main

## Anti-patterns

- Using raw k/n without Wilson
- Blocking buy on full history scan
- Relabeling forever without idempotency (mint unique in dev_launch_labels)
