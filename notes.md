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
- Default `SIGNAL_SOURCE=helius_logs` (create-ix via logsSubscribe; not programSubscribe).
- Jito tip account из env / official tip pubkey (invalid → panic, не `new_unique`).
- `SOLBOT_SKIP_SIMULATE_SELL` — **только paper**; live abort через `assert_safe_for_live`.
- Live hard-guards: `PUBLIC_RPC=true` / `max_signal_age_ms=0` / skip_sim → refuse start.
- Freshness: entry + pre-submit (`assert_signal_still_fresh`).
- Pump.fun ID: `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` (везде едино).
- Strict pool filter: size / PDA / first-seen / warmup 15s.
- DLMM mints @88/120; DAMM v2 pool 1112 B mints @168/200 (не 408 B junk).
- Token-2022 TLV: AccountType @165, resilient truncated entries; official ExtensionType IDs.
- Global stats: `PIPELINE STATS (global cumulative)` (не per-worker).
- DAMM v2 builder + wire actor/simulate_sell.
- create-ix: `logsSubscribe` + getTransaction (`SIGNAL_SOURCE=helius_logs|both`).

## Env (public WS / paper)

```bash
export SIGNAL_SOURCE=helius_ws          # или helius_logs | both
export SOLBOT_PUBLIC_RPC=true
export HELIUS_API_KEY=...
# export SOLBOT_RPC_URL="https://mainnet.helius-rpc.com/?api-key=$HELIUS_API_KEY"

export SOLBOT_MAX_CONCURRENT_SIGNALS=5
export SOLBOT_MAX_SIGNAL_AGE_MS=2000
export SOLBOT_PRE_BUY_TIMEOUT_MS=4000
export SOLBOT_SIMULATE_SELL_TIMEOUT_MS=2000
export SOLBOT_PRE_BUY_RPC_TIMEOUT_MS=2000
export SOLBOT_SIGNAL_WARMUP_SECS=15
export SOLBOT_MAX_QUEUE_DEPTH=64
# export SOLBOT_CREATE_IX=1             # дополнительно к helius_ws
# export SOLBOT_SKIP_SIMULATE_SELL=true # только debug paper
```

Paper:
```bash
cargo run -p solbot-strategy --release -- --paper
```

E2E builders (без broadcast):
```bash
cargo run -p solbot-strategy --release --bin paper_pipeline_e2e
```

## Paper-прогоны (факты)

### ~90s post-filter (2026-08-09)
Profile: `SOLBOT_PUBLIC_RPC=true`, Helius HTTP, budgets 4s/2s, age 2000ms.

| Bucket | Count | Cause |
|---|---|---|
| rpc_timeout | ~104 | Public/free RPS + 2–4s budgets |
| mint_unavailable | 70 | Layout bug DLMM/DAMM — **fixed** |
| DROP_STALE + reject stale | 207+66 | age > 2000ms |
| simulate_sell | 27 | Real Custom() |
| freeze_mint_auth | 11 | Real authority |

### ~8 min (после TLV + offsets)
- ACCEPT **254**, REJECT 1129, DROP_QUEUE **0**
- mint_unavailable **~0–3**
- token2022 parse fail → после TLV fix **0** (остались real blocked extensions)
- queue_depth p50≈0–2, max≪99

### ~2 min polish
- ACCEPT **70**, token2022 fail parse **0**, mint_unavailable **0**
- top reject: rpc_timeout (public) — ожидаемо

## Live hard-guards (abort on start)

Live boot **refuses** if any of:

1. `SOLBOT_SKIP_SIMULATE_SELL=true` — no honeypot sim  
2. `SOLBOT_PUBLIC_RPC=true` — multi-second risk budgets  
3. `max_signal_age_ms=0` — freshness disabled  

Jito tip: invalid `SOLBOT_JITO_TIP_ACCOUNT` → **panic**.  
Pre-submit freshness: buy entry **and** before router submit.

Paper default: `SOLBOT_PUBLIC_RPC=true` если env не задан.  
Live default: `SOLBOT_PUBLIC_RPC=false` если env не задан.

## Program IDs (mainnet)

| DEX | ID |
|---|---|
| Pump.fun | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` |
| Raydium AMM v4 | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` |
| Raydium CPMM | `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C` |
| Raydium CLMM | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` |
| Meteora DLMM v2 | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` |
| Meteora DAMM v2 | `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG` |
| Jito tip default | `96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5` |

## Чеклист после покупки private RPC

Этого **нельзя** закрыть на free/public RPC (latency/RPS), только конфиг + калибровка + live e2e:

- [ ] `SOLBOT_PUBLIC_RPC=false` (**required** for live — иначе abort)
- [ ] `SOLBOT_RPC_URL=<private_http>`
- [ ] `SOLBOT_PRE_BUY_TIMEOUT_MS=600` (цель 150 после co-lo)
- [ ] `SOLBOT_SIMULATE_SELL_TIMEOUT_MS=150`
- [ ] `SOLBOT_PRE_BUY_RPC_TIMEOUT_MS=300`
- [ ] `SOLBOT_MAX_CONCURRENT_SIGNALS=20–30`
- [ ] `SOLBOT_MAX_SIGNAL_AGE_MS=500` (**required** for live)
- [ ] Unset `SOLBOT_SKIP_SIMULATE_SELL`
- [ ] Paper 15–30 мин на private — `rpc_timeout` должен сильно упасть
- [ ] `solbot-signer` up + encrypted key
- [ ] Valid tip или default official
- [ ] Tiny live 1 buy + 1 sell
- [ ] Optional: `SIGNAL_SOURCE=geyser_grpc` + paid endpoint
- [ ] Optional: region-matched Jito
- [ ] Код, который RPC **не** заменяет: дожим DAMM account metas по live fails, DLMM multi-bin amount_out, create-ix V0 parsing, этапы **6–11**

## SIGNAL_SOURCE

| Значение | Поведение |
|---|---|
| `helius_ws` (default) | programSubscribe account updates + strict filter |
| `helius_logs` / `create_ix` | logsSubscribe create/init only + getTransaction |
| `both` / `helius_both` | оба источника |
| `geyser_grpc` | Yellowstone accounts (нужен paid endpoint) |
| `SOLBOT_CREATE_IX=1` | добавить create-ix поверх текущего source |
