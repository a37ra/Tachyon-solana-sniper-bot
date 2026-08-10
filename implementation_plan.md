# Анализ Production-готовности Solana Sniper Bot

## Общая оценка (актуально 2026-08-10)

**Этапы 1–5 (торговый pipeline P0 + technical risk-gate): ~85–90% по логике кода.**  
Paper: create-ix + risk-gate ACCEPT работают.  
**Live money / prod ops: ~40–50%** — private RPC, signer ops, этапы 6–11.  
**Alpha Layer (этапы 12–15): 0%** — ТЗ §8 вписан; код не начат. Без него bot покупает
технически «чистые» dev-dumps (DB→SB→DS).

Устаревшая оценка «~20–25% / всё stub» **снята** для 1–5.

---

## Что уже сделано хорошо ✅

| Область | Статус | Детали |
|---|---|---|
| Архитектура и декомпозиция | ✅ Готово | 6 крейтов, чёткое разделение ответственности |
| Protobuf-контракт `signer.proto` | ✅ Готово | Полностью соответствует ТЗ 5.2 |
| SQLite DDL / миграции | ✅ Готово | 2 миграции, схема по ТЗ 5.1 |
| Two-Lane Actor модель | ✅ Готово | `biased select!`, control/market data lanes, deadlines |
| Escalation State Machine (2.3) | ✅ Реализована | Полная forward-only FSM с юнит-тестами |
| Exit Strategy (4.4) | ✅ Логика готова | TP ladder, trailing stop, hard SL — с тестами |
| Circuit Breaker (4.8) | ✅ Базово | Per-tier thresholds из Appendix A |
| Adaptive Sizing (4.1) | ✅ Базово | Kelly-калибровка, min_sample_size=30, window=100 |
| Signer IPC Server | ✅ Работает | gRPC over UDS, real `sign_transaction`, validation, audit_log, sequence_ledger |
| Sequence Ledger (anti-replay) | ✅ Реализован | Hydrate → cache → atomic update в SQLite |
| Compromise Detection (3.3) | ✅ Базово | Sliding window rate limiter |
| Telegram Alerts (7.1) | ✅ Форматирование | MarkdownV2 templates (HTTP send — этап 8) |
| Event Bus (BotEvent) | ✅ Готово | 11 типов событий, serde serializable |
| Config YAML | ✅ Готово | base, escalation, risk_gate, mev, otel, tiers |
| Deploy скелет | ✅ Каркас | systemd units, litestream, vault policy, sysctl |
| CLI каркас | ✅ Базово | kill-switch, position, circuit-breaker commands |
| DEX listeners (WS/Geyser) | ✅ Работает | Helius programSubscribe + strict pool filter + warmup/first-seen |
| create-ix signals | ✅ Добавлено | `logsSubscribe` + getTransaction (`SIGNAL_SOURCE=helius_logs\|both`) |
| Transaction builders | ✅ Работает | Pump, Raydium AMM/CPMM/CLMM, Meteora DLMM, DAMM v2, Dynamic |
| Risk gate | ✅ Работает | mint/freeze, LP, Token-2022 TLV, simulate_sell + classifier |
| Routing | ✅ Работает | Jito + RPC fallback + durable nonce module |
| Actor ↔ pipeline | ✅ Работает | NewPoolSignal → buy; exit/escalation sell; paper/live |
| Global pipeline stats | ✅ Работает | `PIPELINE STATS (global cumulative)` |
| Live hard-guards | ✅ Работает | skip_sim / public_rpc / max_age / Jito tip fail-closed |

---

## Что ещё НЕ готово / частично ❌

### Этапы 6–11 (P1/P2)
- PnL net, WSOL unwrap, smart money — **stub / partial**
- Full hydration / crash recovery — **partial**
- OTel / Telegram HTTP / Prometheus — **config/templates, не fully wired**
- CLI live ops — **каркас**
- Chaos/property/load production-grade — **слабо**
- Vault agent / litestream / calibration — **скелет**

### Остатки 1–5 (после private RPC / live)
- Vault/KMS API (сейчас encrypted file / JSON key — ок для dev)
- Property-fuzz 100k intents (слабый)
- Create-ix resolve на V0 tx — heuristic; улучшать по live логам
- DLMM multi-bin pricing всё ещё single-bin approximation для amount_out
- DAMM v2 account meta list — по mainnet IDL может потребовать доп. accounts
- Tiny live e2e buy/sell на private RPC
- Co-lo / region Jito calibration

---

## Поэтапный план реализации

> Этапы в порядке критического пути. Чекбоксы отражают **текущий код**, не исходный аудит.

---

### Этап 1: Фундамент — Signer + Key Material + Transaction Builder ✅

#### 1.1 Key Material
- [x] `load_keypair()` encrypted file / JSON / base58 (не `Keypair::new()` в hot path)
- [x] `mlock` для памяти с ключом
- [x] Запрет core dump (systemd `LimitCORE=0`)
- [ ] Vault/KMS production API (этап 11)

#### 1.2 Transaction Builder
- [x] Raydium AMM v4
- [x] Raydium CP-MM
- [x] Raydium CLMM (tick arrays via bitmap)
- [x] Meteora DLMM (bin arrays current±adjacent; layout mints@88/120)
- [x] Meteora Dynamic AMM (legacy Eo7…)
- [x] Meteora DAMM v2 (cpamdp…, pool 1112 B, mints@168/200)
- [x] Pump.fun bonding curve
- [x] Compute Budget + blockhash cache + ATA
- [x] Интеграционные / e2e builder smoke (`paper_pipeline_e2e`)

#### 1.3 Signer IPC
- [x] Реальный `sign_transaction()` (не dummy_b64)
- [x] hard_cap_bps / destination whitelist validation
- [ ] Property-fuzz 100k intents (proptest) — отложено

---

### Этап 2: Signal Source — DEX Listeners ✅ (paper / public WS)

#### 2.1 Geyser/gRPC + WebSocket
- [x] Yellowstone/Geyser client (accounts owner filter)
- [x] Helius WebSocket `programSubscribe` (default `SIGNAL_SOURCE=helius_ws`)
- [x] Фильтры Raydium / Meteora / Pump program IDs
- [x] Strict pool filter: size bands, PDA (pump), first-seen, warmup
- [x] create-ix path: `logsSubscribe` + getTransaction (`helius_logs` / `both` / `SOLBOT_CREATE_IX=1`)

#### 2.2 Signal Parsing
- [x] mint / pool / dex_program extraction
- [x] `max_signal_age_slots` + `SignalValidator` (default 30)
- [x] Dedup filter

#### 2.3 Reconnection & Backfill
- [x] Auto-reconnect + backoff (WS/Geyser)
- [x] ExtendedHistoryProvider (partial backfill)
- [ ] Полный backfill слотов после reconnect на всех источниках

#### Заметки этапа 2 (история, актуализировано)
1. ~~geyser filter TEMPORARY без account_include~~ — **[ГОТОВО]** accounts-based + owner filter.
2. ~~Transaction-ветка с dummy AccountUpdate~~ — **[ГОТОВО]** убрана / debug.
3. Сигнальная логика dex_listeners — **содержательная**, не sleep-loop.
4. Helius Free не даёт Geyser stream — **инфра**; paper идёт через `helius_ws`.
5. После paid Geyser: `SIGNAL_SOURCE=geyser_grpc` + `dex_listener_debug` / paper 2–5 мин.

---

### Этап 3: Risk Gate — Real Pre-Buy Checks ✅

#### 3.1–3.4
- [x] Mint/freeze authority (Token-2022 base 82 bytes)
- [x] LP lock (+ N/A для CLMM/DLMM/DAMM/pump)
- [x] Token-2022 TLV (AccountType@165, resilient truncated TLV)
- [x] simulate_sell + honeypot vs dummy-wallet classifier
- [x] Shared RpcClient, coalescer, WS pool fast path
- [x] Public profile timeouts 4s/2s; private profile 600/150

#### 3.5 hard_timeout
- [x] `run_pre_buy_checks_with_timeout` (env `SOLBOT_PRE_BUY_TIMEOUT_MS`)
- [ ] KPI 150ms end-to-end — **только с private/co-lo RPC** (см. notes.md)

---

### Этап 4: Routing — Jito + Public RPC + Durable Nonce ✅

- [x] Jito bundle + tip (official tip default; invalid env → panic)
- [x] Public RPC send + confirmation
- [x] Durable nonce module (default off)
- [x] TransactionRouter AutoFallback
- [ ] Region-best Jito + tip calibration (private infra)

---

### Этап 5: Actor ↔ Pipeline ✅

- [x] Buy: risk → sizing → build → sign → route
- [x] Sell: exit evaluate → escalation → sign → route
- [x] NewPoolSignal + raw_pool bytes; Accept-only dispatch
- [x] Paper vs Live; freshness pre-submit
- [x] Position lifecycle + SQLite hooks
- [x] Circuit breaker hooks
- [x] DAMM/CLMM/DLMM/Pump wired in `fetch_and_build_dex_swap`
- [ ] Tiny live e2e on private RPC + signer

---

### Этап 6: PnL, WSOL, Smart Money (~1 неделя)

- [ ] Track priority fees, Jito tips per position
- [ ] Track ATA rent / refund
- [ ] `realized_pnl_net_sol`
- [ ] Auto-unwrap WSOL / CloseAccount
- [ ] Smart money watchlist + modes

---

### Этап 7: Startup & Recovery (~1 неделя)

- [ ] Full hydration ТЗ 2.4 (positions + in-flight + backfill + resume)
- [ ] Crash recovery / idempotent sell resume
- [x] Частичная hydration skeleton уже есть

---

### Этап 8: Observability & Alerting (~1 неделя)

- [ ] OTel spans/metrics fully wired
- [ ] Telegram HTTP client + rate limit
- [ ] Email SMTP fallback
- [ ] Prometheus `/metrics`
- [x] Templates / YAML configs

---

### Этап 9: CLI (~3–5 дней)

- [ ] kill-switch / position / CB / mode / wallet против live process
- [x] Каркас команд

---

### Этап 10: Тесты Production-Grade (~1.5–2 недели)

- [ ] Chaos (signer kill, RPC 429, Jito down)
- [ ] Property 100k intents
- [ ] Load + latency waterfall
- [x] Unit/integration: risk_gate, pool_signal, token2022, actor loop, builders
- [x] `paper_pipeline_e2e` bin

---

### Этап 11: Deploy & Hardening (~1 неделя)

- [ ] systemd units filled + vault-agent
- [ ] Litestream restore test
- [ ] sysctl / BBR
- [ ] Calibration runbook (RTT, pre-buy under load, paper→30 closes)

---

---

### Этап 12: Alpha Data Collection — bundle/funder/concentration (~1–1.5 нед) ⬜

> ТЗ §8.2, formulas: `docs/alpha/ALPHA_LAYER_FORMULAS.md`, prompt: `docs/agent_prompts/stage_12_alpha_data_collection.md`

- [ ] `alpha/bundle_detector.rs` — bundler_pct
- [ ] `alpha/funder_graph.rs` — funder_cluster_pct, fresh_wallet_ratio (2-hop + cache)
- [ ] `alpha/concentration.rs` — top10_holder_pct, CM
- [ ] `state/alpha_signals_repo.rs` + migration `0004_alpha_layer.sql`
- [ ] Snapshot **всех** решений (вкл. hard_block) в `alpha_signals`
- [ ] Unit-тесты synthetic bundle/funder

---

### Этап 13: Alpha Dev History — Wilson + labeling (~1 нед) ⬜

> ТЗ §8.3, prompt: `docs/agent_prompts/stage_13_alpha_dev_history.md`

- [ ] `alpha/dev_history.rs` — Wilson lower bound
- [ ] `alpha/labeling_worker.rs` — dump/not_dump after label_delay_min
- [ ] Creator extract per DEX (pump curve creator@49, …)
- [ ] `state/dev_wallet_history_repo.rs`
- [ ] Integration test n=1..40 convergence

---

### Этап 14: Composite Gate + Probe-Scale (~1.5–2 нед) ⬜

> ТЗ §8.4–8.5, prompt: `docs/agent_prompts/stage_14_alpha_gate_probe.md`

- [ ] `alpha/composite_gate.rs` — hard_blocks + soft score → size_multiplier
- [ ] Wire **before/parallel** risk_gate; span `alpha_gate_latency_ms`
- [ ] `alpha/probe_scale.rs` — probe→confirm same position_id VWAP
- [ ] Probe dump → emergency_exit 4th trigger
- [ ] `config/alpha_layer.yaml`
- [ ] E2E paper: probe→confirm and probe→emergency

---

### Этап 15: Alpha Calibration (~1–2 нед after data) ⬜

> ТЗ §8.4/8.5/Прил.C, prompt: `docs/agent_prompts/stage_15_alpha_calibration.md`

- [ ] ≥150–200 labels/class in `alpha_signals.outcome_label`
- [ ] Fit w1..w5; measure δ; recompute p*
- [ ] FP rate hard-blocks; tune hard_max_pct
- [ ] Clear flag `alpha_weights_not_calibrated`

---

## Сводка по приоритетам

| Приоритет | Этапы | Статус |
|---|---|---|
| 🔴 P0 | 1–5 | **Technical pipeline done; live needs private RPC** |
| 🟠 P0-alpha | 12–14 | **Alpha Layer — must before meaningful live on pump.fun** |
| 🟡 P1 | 6–8 | PnL / recovery / observability |
| 🟢 P2 | 9–11, 15 | CLI, tests, deploy, calibration |

---

## Открытые вопросы

1. **Geyser provider** для live: Helius LaserStream / Chainstack / свой валидатор?
2. **Vault/KMS**: HashiCorp / AWS / encrypted file (сейчас file)?
3. **Jito**: region + tip strategy под co-lo?
4. **Devnet profile** отдельным yaml?
5. **SDK**: solana 2.x future-incompat — план апдейта?

---

## ЗАМЕТКИ (сохранено, актуализировано)

### Этап 1
- Vault/KMS API — не блокер этапа 1 (encrypted file допустим); для боя — этап 11.
- Property-fuzz 100k — отложено до этапа 10.

### Этап 2
- geyser filter + Transaction dummy — **исправлено**.
- Helius Free = 0 Geyser updates — **инфра**, не баг listeners.
- Paper: `helius_ws` + strict filter + warmup; create-ix: `helius_logs` / `both`.
- Проверка: `cargo run -p solbot-strategy --release -- --paper` + смотреть `PIPELINE STATS`.

### Latency / anti-rug (2026-08-09+)
См. `notes.md` — корневые причины таймаутов и фиксы (shared RPC, simulate context, coalescer, pump ID, raw_pool, TLV, offsets DLMM/DAMM).
