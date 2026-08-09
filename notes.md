# HFT Infrastructure & Risk Gate notes

## Почему anti-rug «всегда таймаутил» (исправлено 2026-08-09)

Корневые причины (не «медленный код фильтров»):

1. **simulate_sell** делал лишние 2–3 RTT (get_account pool + blockhash + simulate),
   даже когда pool bytes уже были в контексте / с WS.
2. **Timeout budget**: `pre_buy=600ms` + `simulate=150ms` при public RPC RTT 200–800ms —
   математически невыполнимо (3×RTT > budget).
3. **Fallback storm**: при timeout fetch запускались 4 параллельных individual checks →
   rate-limit public RPC → ещё больше timeout.
4. **Новый RpcClient на каждый сигнал** в paper (нет keep-alive) — +30–60ms TLS каждый раз.
5. **Неверный Pump.fun program id** в simulate_sell → unsupported DEX / лишние ошибки.
6. **Coalescer race**: два waiter могли оба ходить в RPC без `claimed` флага.
7. **Actor drop raw_pool**: buy pipeline обнулял WS bytes → всегда 2-RTT path.

### Что сделано
- Shared `RpcClient` + `RiskGateConfig::from_shared_rpc` / `resolve_rpc_url()` (Helius HTTP key).
- `simulate_sell::check_with_context` — 1 RTT (blockhash+sim) если pool уже есть.
- Fail-closed без multi-RPC fallback storm.
- Public profile defaults: `pre_buy=4000ms`, `simulate=2000ms`, RPC call=2000ms.
- Coalescer с atomic claim.
- `NewPoolSignal` → actor buy path с raw pool bytes.
- Default `SIGNAL_SOURCE=helius_ws`.
- Jito tip account из env / official tip pubkey.
- `SOLBOT_SKIP_SIMULATE_SELL=true` для paper без sim RTT.

## Env (public WS testing)

```bash
export SIGNAL_SOURCE=helius_ws
export SOLBOT_PUBLIC_RPC=true
export HELIUS_API_KEY=...          # или GEYSER_X_TOKEN
# HTTP RPC предпочтительно тот же Helius (не api.mainnet-beta):
# export SOLBOT_RPC_URL="https://mainnet.helius-rpc.com/?api-key=$HELIUS_API_KEY"

export SOLBOT_MAX_CONCURRENT_SIGNALS=3
export SOLBOT_MAX_SIGNAL_AGE_MS=2000
export SOLBOT_PRE_BUY_TIMEOUT_MS=4000
export SOLBOT_SIMULATE_SELL_TIMEOUT_MS=2000
export SOLBOT_PRE_BUY_RPC_TIMEOUT_MS=2000
# optional: skip heaviest RTT while debugging filters only
# export SOLBOT_SKIP_SIMULATE_SELL=true
```

Paper:
```bash
SIGNAL_SOURCE=helius_ws cargo run -p solbot-strategy --release -- --paper
```

## Paper 90s post-filter diagnosis (2026-08-09)

**Profile of that run:** `SOLBOT_PUBLIC_RPC=true` (paper default), Helius HTTP via
`SOLBOT_RPC_URL`, budgets simulate=2000ms / pre_buy=4000ms, `max_signal_age_ms=2000`.

| Bucket | Count | Cause |
|---|---|---|
| rpc_timeout | ~104 | Public/free RPS + 2–4s budgets under concurrent checks — **not** a new logic bug |
| mint_unavailable | 70 | **Layout bug** DLMM mint@72/104 (real @88/120); DAMM v2 408B junk + wrong offsets — **fixed** |
| DROP_STALE + reject stale | 207+66 | Threshold **2000ms**; ages p50≈2562 p90≈3398 — wait in semaphore + check latency |
| simulate_sell | 27 | Real Custom() honeypot-ish rejects |
| freeze_mint_auth | 11 | Real authority checks (e.g. USDC) |

Global stats: `signal/pipeline_stats.rs` → log line `PIPELINE STATS (global cumulative)`.

## Live hard-guards (abort on start)

Live boot **refuses** if any of:

1. `SOLBOT_SKIP_SIMULATE_SELL=true` — no honeypot sim
2. `SOLBOT_PUBLIC_RPC=true` — multi-second risk budgets
3. `max_signal_age_ms=0` / missing `SOLBOT_MAX_SIGNAL_AGE_MS` with public defaults unset for live

Jito tip: invalid `SOLBOT_JITO_TIP_ACCOUNT` → **panic** (never random pubkey / silent SOL burn).

Pre-submit freshness: `assert_signal_still_fresh` runs at buy pipeline entry **and**
immediately before router submit (`max_signal_age_ms` from risk config).

## Чеклист при private RPC / paid Geyser

- [ ] `SOLBOT_PUBLIC_RPC=false` (**required** for live)
- [ ] `SOLBOT_PRE_BUY_TIMEOUT_MS=600` (цель 150 после co-lo)
- [ ] `SOLBOT_SIMULATE_SELL_TIMEOUT_MS=150`
- [ ] `SOLBOT_PRE_BUY_RPC_TIMEOUT_MS=300`
- [ ] `SOLBOT_MAX_CONCURRENT_SIGNALS=30`
- [ ] `SOLBOT_MAX_SIGNAL_AGE_MS=500` (**required** for live)
- [ ] `SIGNAL_SOURCE=geyser_grpc` + paid endpoint
- [ ] Valid `SOLBOT_JITO_TIP_ACCOUNT` or omit (uses official default)
