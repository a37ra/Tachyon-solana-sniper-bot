# Agent Prompt — Этап 10: Production-Grade Tests

## Context

Unit/integration tests exist for risk_gate, pool_signal, builders. Need chaos/property/load per TZ §7.3.

## Goal

- Chaos: signer kill -9 mid-intent; RPC 429; Jito down → escalation path
- Property: 100k mutated SignerIntent → 100% reject invalid
- Load: latency waterfall under max_concurrent_positions
- Control-lane starvation: price_tick storm must not delay migration_event beyond deadline
- Durable-nonce reuse: stage_3 retries without new signer audit rows

## Constraints

- Tests must not require mainnet funds.
- Prefer deterministic clocks/fakes over flaky sleeps.
- Do not disable live hard-guards in production code to make tests pass.

## Deliverables

- [ ] Chaos suite under `tests/` or crate `tests/`
- [ ] Property fuzz (proptest) on intent validation
- [ ] Document how to run in `docs/RUNBOOK.md`
- [ ] Plan checkboxes

## Verification

```bash
cargo test --workspace
# optional: cargo test -p solbot-signer --features chaos
```

## Hints

- Start from existing escalation unit tests and expand.
- Mock RpcClient / Jito client traits if not already abstracted — introduce thin traits only if needed.
