# Agent Prompt — Этап 7: Startup & Crash Recovery

## Context

Implement **full hydration** TZ §2.4. Partial skeleton exists in `startup/hydration.rs`. Stages 1–5 trading path works; recovery must not break paper/live boot.

**Sources:** TZ §2.4, §2.3 stage_3 durable nonce; `implementation_plan.md` Этап 7; file tree `startup/`.

## Goal

On process start, in order:

1. Load `ipc_sequence_ledger` **before** any signer intents (if strategy talks to signer)
2. Ensure signer process is up
3. Load positions from SQLite; reconcile token account balances on-chain
4. Resolve in-flight `sending` txs (last ~150 slots / blockhash window)
5. Backfill missed slots via extended history provider
6. Resume Geyser/WS subscriptions
7. Resume stuck sells via escalation FSM (idempotent)

## Constraints

- Fail-closed on ledger hydrate failure (no silent zero last_ipc_seq).
- Do not double-open positions for same mint if already open.
- Crash mid-sell: resume from last known escalation stage, do not reset to stage_0 blindly if durable-nonce stage_3 already entered.

## Deliverables

- [ ] Complete `startup/hydration.rs` order matching TZ 2.4
- [ ] In-flight tx checker
- [ ] Idempotent sell resume hooks into `exit/escalation.rs`
- [ ] Integration/chaos-style unit test: kill mid-state → hydrate → no duplicate position
- [ ] Checkbox update in `implementation_plan.md`

## Do NOT

- Rewrite signal create-ix path
- Implement alpha layer
- Soften live hard-guards (public_rpc / skip_sim)

## Verification

```bash
cargo test -p solbot-strategy --lib hydration startup
# manual: start --paper twice; positions table consistent
```

## Hints

- Read existing `hydrate_system_state` call sites in `main.rs`.
- `ipc_sequence_ledger` lives in **signer** process DB path — strategy may only wait/healthcheck.
- Prefer small pure functions for reconcile logic (easy to test).
