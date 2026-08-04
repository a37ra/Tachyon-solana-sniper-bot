# Анализ Production-готовности Solana Sniper Bot

## Общая оценка: ~20-25% готовности к проду

Проект представляет собой **добротный архитектурный скелет** с правильной модульной декомпозицией, но подавляющее большинство модулей — это заглушки (placeholder / stub / dummy), которые компилируются и проходят тривиальные тесты, но **не выполняют реальной работы**. Ни одна транзакция не может быть реально собрана, подписана или отправлена в сеть Solana.

---

## Что уже сделано и сделано хорошо ✅

| Область | Статус | Детали |
|---|---|---|
| Архитектура и декомпозиция | ✅ Готово | 6 крейтов, чёткое разделение ответственности |
| Protobuf-контракт `signer.proto` | ✅ Готово | Полностью соответствует ТЗ 5.2 |
| SQLite DDL / миграции | ✅ Готово | 2 миграции, схема по ТЗ 5.1 |
| Two-Lane Actor модель | ✅ Каркас готов | `biased select!`, control/market data lanes, deadlines |
| Escalation State Machine (2.3) | ✅ Реализована | Полная forward-only FSM с юнит-тестами (504 строки) |
| Exit Strategy (4.4) | ✅ Логика готова | TP ladder, trailing stop, hard SL — с тестами |
| Circuit Breaker (4.8) | ✅ Базово | Per-tier thresholds из Appendix A |
| Adaptive Sizing (4.1) | ✅ Базово | Kelly-калибровка, min_sample_size=30, window=100 |
| Signer IPC Server | ✅ Каркас | gRPC over UDS, validation, audit_log, sequence_ledger |
| Sequence Ledger (anti-replay) | ✅ Реализован | Hydrate → cache → atomic update в SQLite |
| Compromise Detection (3.3) | ✅ Базово | Sliding window rate limiter |
| Telegram Alerts (7.1) | ✅ Форматирование | MarkdownV2 templates для всех 11 типов событий |
| Event Bus (BotEvent) | ✅ Готово | 11 типов событий, serde serializable |
| Config YAML | ✅ Готово | base, escalation, risk_gate, mev, otel, tiers |
| Deploy скелет | ✅ Каркас | systemd units, litestream, vault policy, sysctl |
| CLI каркас | ✅ Базово | kill-switch, position, circuit-breaker commands |

## Что является заглушками / НЕ работает ❌

> [!CAUTION]
> Ниже перечислены критические области, где код — это **placeholder** с `tracing::info!()` вместо реальной логики. Каждая из них — обязательное условие для прода.

### 1. DEX Listeners (Signal Source) — 0% реальной логики
- [raydium.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-strategy/src/signal/dex_listeners/raydium.rs) — пустой `loop` с `sleep(10s)`
- [meteora.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-strategy/src/signal/dex_listeners/meteora.rs) — аналогично
- [pump_fun.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-strategy/src/signal/dex_listeners/pump_fun.rs) — аналогично
- **Нет Geyser/gRPC подписки, нет парсинга программных инструкций**

### 2. Transaction Builder — отсутствует полностью
- Нет модуля построения swap-инструкций (Raydium, Meteora, Pump.fun)
- Нет compute budget instructions
- Нет cached blockhash
- `unsigned_transaction_base64` нигде не формируется

### 3. Signing — dummy
- [signing.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-signer/src/signing.rs) содержит рабочий `sign_transaction()`, **но он никогда не вызывается**
- В [ipc_server.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-signer/src/ipc_server.rs#L91) строка 91: `let (signed, sig) = ("dummy_b64".to_string(), "dummy_sig".to_string())`

### 4. Key Material — dummy
- [vault_kms_client.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-signer/src/key_material/vault_kms_client.rs) — `Keypair::new()` вместо загрузки из Vault/KMS
- `mlock` не реализован (только комментарий в main.rs)

### 5. Routing — все три маршрута placeholder
- [jito_bundle.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-strategy/src/routing/jito_bundle.rs) — `sleep(100ms)` + dummy result
- `public_rpc_fallback.rs` — аналогичная заглушка
- `durable_nonce.rs` — только структура

### 6. Risk Gate Checks — частично
- [simulate_sell.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-strategy/src/risk_gate/simulate_sell.rs) — делает `get_account()`, но **не строит и не симулирует sell-транзакцию**
- `mint_freeze_authority.rs`, `lp_lock_check.rs`, `token2022_extensions.rs` — структуры есть, но RPC-вызовы и парсинг аккаунтов не реализованы

### 7. Startup Hydration — stub
- [hydration.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-strategy/src/startup/hydration.rs) — 6 строк `tracing::info!()` вместо реальной гидратации

### 8. Actor ↔ Trading Pipeline — не связан
- Actor получает события, но **не вызывает risk_gate, sizing, signer_client, routing**
- Mint actor только логирует события

### 9. Signer Client — placeholder
- [signer_client/mod.rs](file:///home/astra/Проекты/solana-bot/crates/solbot-strategy/src/signer_client/mod.rs) — возвращает dummy response, **нет реального gRPC-вызова через UDS**

### 10. PnL Net — stub
- Нет реального расчёта P&L с учётом fees, tips, ATA rent

### 11. WSOL Management — stub
- Нет auto-unwrap логики, нет `CloseAccount` для rent reclamation

### 12. Smart Money — stub
- Модули `subscription.rs`, `consumption.rs`, `mode.rs` — только структуры

### 13. Тесты — тривиальные
- Chaos тесты — `assert!(true)` (7 строк каждый)
- Property тест — `assert!(69 > 68)` (7 строк)
- Load тесты — **пустая директория**
- Реальные юнит-тесты есть только в `escalation.rs` и `exit/mod.rs`

### 14. Email Backup — stub
- Только `tracing::info!()`, нет SMTP-клиента

### 15. Observability — не интегрирована
- YAML конфиги есть, но **нет OTel SDK интеграции в коде** (spans, metrics, histograms)

---

## Поэтапный план реализации

> [!IMPORTANT]
> Этапы расположены в порядке критического пути: каждый следующий зависит от предыдущих. Примерные трудозатраты указаны для ориентира.

---

### Этап 1: Фундамент — Signer + Key Material + Transaction Builder (~2-3 недели)

Без этого этапа невозможна ни одна реальная транзакция.

#### 1.1 Key Material — загрузка из Vault/KMS
- [ ] Реализовать `load_keypair()` с реальным Vault API (или encrypted file + env)
- [ ] Реализовать `mlock` для памяти с ключом
- [ ] Запрет core dump (systemd `LimitCORE=0` уже есть, но нужен runtime check)

#### 1.2 Transaction Builder
- [ ] Модуль построения Raydium AMM v4 swap instructions
- [ ] Модуль построения Raydium CP-MM swap instructions
- [ ] Модуль построения Raydium CLMM swap instructions (tick arrays)
- [ ] Модуль построения Meteora DLMM swap instructions (bin arrays)
- [ ] Модуль построения Meteora Dynamic AMM swap instructions
- [ ] Модуль построения Pump.fun bonding curve swap instructions
- [ ] Compute Budget Instructions (set compute units + priority fee)
- [ ] Cached blockhash provider (с автообновлением)
- [ ] ATA creation / lookup (Associated Token Account)
- [ ] Базовые интеграционные тесты на devnet

#### 1.3 Signer IPC — подключение реального signing
- [ ] Заменить `"dummy_b64"/"dummy_sig"` на вызов `sign_transaction()` из `signing.rs`
- [ ] Добавить `hard_cap_bps` валидацию (разбор unsigned tx, проверка slippage)
- [ ] Добавить `destination_addresses` whitelist валидацию (unexpected_destination)
- [ ] Тест: property-fuzzing 100k мутированных intent'ов

---

### Этап 2: Signal Source — DEX Listeners (~1.5-2 недели)

#### 2.1 Geyser/gRPC подписка
- [] Интеграция с Yellowstone/Geyser gRPC (или WebSocket)
    API: f22a2b7e-479a-4040-847e-66337dc830e4
- [] Парсинг account update notifications по program_id
- [] Фильтр по Raydium AMM v4 (`675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`)
- [] Фильтр по Raydium CP-MM, CLMM
- [] Фильтр по Meteora DLMM, Dynamic AMM
- [] Фильтр по Pump.fun bonding curve + migration event
    1 миллион кредитов — это много, но если ты подпишешься на вообще все транзакции в сети, лимит сгорит за час.
Поэтому в SubscribeRequest обязательно настрой фильтр account_include или vote: false, чтобы Helius присылал тебе только транзакции Pump.fun или Raydium, а не весь мусор сети.
#### 2.2 Signal Parsing
- [] Парсинг InitializePool / Swap инструкций для каждого DEX
- [] Извлечение mint, pool_address, dex_program_id
- [] `max_signal_age_slots: 30` — проверка slot age

#### 2.3 Reconnection & Backfill
- [] Auto-reconnect при разрыве WebSocket/gRPC
- [] Backfill пропущенных слотов через `extended_history_provider`

---

### Этап 3: Risk Gate — Real Pre-Buy Checks (~1-1.5 недели)

#### 3.1 Mint/Freeze Authority Check
- [ ] Реальный `getAccountInfo` → парсинг Mint account → проверка `mint_authority == None && freeze_authority == None`

#### 3.2 LP Lock Check
- [ ] Проверка LP token mint → owner = burn address или timelock program

#### 3.3 Token-2022 Extensions
- [ ] Парсинг TLV extension data из Token-2022 mint account
- [ ] Allowlist/Blocklist matching (`transfer_fee`, `permanent_delegate`, `transfer_hook`)

#### 3.4 Simulate Sell Before Buy
- [ ] Построение sell-транзакции для `simulateTransaction`
- [ ] Парсинг result logs → детекция honeypot (revert / excessive fee / zero output)
- [ ] Hard timeout 80ms с fallback reject

#### 3.5 hard_timeout_ms: 150
- [ ] Обёртка `tokio::time::timeout(150ms, run_pre_buy_checks())`

---

### Этап 4: Routing — Jito + Public RPC + Durable Nonce (~1-1.5 недели)

#### 4.1 Jito Bundle Submission
- [ ] HTTP/gRPC клиент к Jito block engine
- [ ] Bundle formation (signed tx + tip instruction)
- [ ] Randomized micro-jitter для tip
- [ ] Bundle status polling / confirmation
- [ ] Auto-region selection по RTT

#### 4.2 Public RPC Direct Submit
- [ ] `sendTransaction` с настройкой `skipPreflight`
- [ ] Confirmation polling (`getSignatureStatuses`)
- [ ] Priority fee estimation (`getRecentPrioritizationFees`)

#### 4.3 Durable Nonce
- [ ] Create nonce account (скрипт уже есть в `scripts/`)
- [ ] AdvanceNonceAccount instruction в транзакцию
- [ ] Infinite retry с переиспользованием подписи

---

### Этап 5: Actor ↔ Pipeline Интеграция (~1.5-2 недели)

#### 5.1 Actor Trading Loop
- [ ] При получении нового пула → `run_pre_buy_checks()` → `sizing::compute()` → build tx → `signer_client.sign_intent()` → `routing.submit()`
- [ ] При price tick → `exit::evaluate_exit()` → trigger TP/SL/trailing

#### 5.2 Signer Client — реальный gRPC вызов
- [ ] `tonic` клиент через `UnixStream` к `/tmp/solbot-signer.sock`
- [ ] Timeout, retry, error handling

#### 5.3 Position Lifecycle
- [ ] `Open → TP1_filled → TP2_filled → TP3_filled → Closed`
- [ ] `Open → Closing → Closed` (SL / trailing)
- [ ] `Open → stuck_manual_intervention_required`
- [ ] State transitions → SQLite + event bus

#### 5.4 Circuit Breaker Integration
- [ ] Hook в trading loop: `on_loss → circuit_breaker.record_loss() → check status`
- [ ] При `limit_triggered` → emergency exit escalation для всех позиций тира
- [ ] Daily reset cron (00:00 UTC)

---

### Этап 6: PnL, WSOL, Smart Money (~1 неделя)

#### 6.1 P&L Net Calculation
- [ ] Track priority fees, Jito tips per position
- [ ] Track ATA rent / refund
- [ ] `realized_pnl_net_sol` = gross PnL - fees - tips + rent_refund
- [ ] On-chain reconciliation

#### 6.2 WSOL Management
- [ ] Auto-unwrap WSOL при balance > 0.1 SOL
- [ ] `CloseAccount` для пустых Token Accounts → rent reclamation
- [ ] Учёт возвращённой ренты в P&L

#### 6.3 Smart Money Watchlist
- [ ] Geyser подписка на транзакции N адресов из `wallet_watchlist`
- [ ] Dedup по `(wallet_address, mint, tx_signature)`
- [ ] Emit `SmartMoneyHit` event → event bus
- [ ] `tag_only` / `soft_boost` / `hard_gate` mode switching

---

### Этап 7: Startup & Recovery (~1 неделя)

#### 7.1 Full Startup Hydration (ТЗ 2.4)
- [ ] Шаг 1: `ipc_sequence_ledger` hydrate (уже в signer)
- [ ] Шаг 2: Запуск signer (systemd dependency)
- [ ] Шаг 3: Загрузка positions из SQLite + on-chain reconciliation
- [ ] Шаг 4: In-flight check (последние 150 слотов)
- [ ] Шаг 5: Backfill через `extended_history_provider`
- [ ] Шаг 6: Resume Geyser/RPC subscriptions

#### 7.2 Crash Recovery
- [ ] Idempotent sell: при рестарте проверить `sending` позиции
- [ ] Resume stuck escalation (Stage 3 durable nonce retry)

---

### Этап 8: Observability & Alerting (~1 неделя)

#### 8.1 OpenTelemetry Integration
- [ ] `tracing-opentelemetry` для spans (signal_received → bundle_submitted)
- [ ] Histogram metrics: `risk_gate_latency_ms`, `submission_engine_latency_ms`, `end_to_end_latency_ms`
- [ ] Counter metrics: все 9 метрик из ТЗ 7.2

#### 8.2 Telegram Bot Client
- [ ] Реальный HTTP-клиент к Telegram Bot API
- [ ] MarkdownV2 форматирование (templates уже готовы)
- [ ] Rate limiting

#### 8.3 Email Backup
- [ ] SMTP клиент (`lettre` crate)
- [ ] Fallback при недоступности Telegram

#### 8.4 Prometheus Metrics
- [ ] `/metrics` endpoint для Grafana

---

### Этап 9: CLI Полноценный (~3-5 дней)

- [ ] `solbot-cli kill-switch on|off` — реальный UDS/файловый сигнал
- [ ] `solbot-cli position list|show|force-close` — реальные SQLite запросы
- [ ] `solbot-cli circuit-breaker status|reset`
- [ ] `solbot-cli mode paper|live`
- [ ] `solbot-cli wallet balance|rotate`

---

### Этап 10: Тесты Production-Grade (~1.5-2 недели)

#### 10.1 Chaos Tests (ТЗ 7.3)
- [ ] SIGKILL signer → restart → property-fuzz replay protection
- [ ] Control-lane starvation: >10k price_tick/sec + migration_event → deadline check
- [ ] Escalation full-path: Jito unavailable + RPC unavailable → Stage 3 stuck
- [ ] Durable-nonce reuse: >5 min retry → 0 new signer calls
- [ ] SIGKILL во время sell → idempotency check

#### 10.2 Property Tests
- [ ] Signing-rate threshold: ≤ порог → no alarm, > порог → alarm
- [ ] 100k мутированных intent'ов → 100% reject невалидных

#### 10.3 Load Tests
- [ ] max_concurrent_positions под полной нагрузкой
- [ ] Latency waterfall verification (p50/p99 против KPI)

#### 10.4 Integration Tests
- [ ] Devnet round-trip: build → sign → submit → confirm
- [ ] Paper mode: полный цикл без реальных SOL

---

### Этап 11: Deploy & Hardening (~1 неделя)

#### 11.1 Systemd Hardening
- [ ] Заполнить systemd units реальными бинарями и зависимостями
- [ ] `solbot-signer.service` → `After=vault-agent.service`
- [ ] `solbot-strategy.service` → `After=solbot-signer.service`

#### 11.2 Vault/KMS Integration
- [ ] Vault agent sidecar для key material
- [ ] Secret rotation для Telegram token, RPC API keys

#### 11.3 Litestream
- [ ] S3/R2 backend конфигурация
- [ ] Restore тест

#### 11.4 sysctl Tuning
- [ ] Применить `99-solbot.conf` на target server
- [ ] BBR congestion control verification

#### 11.5 Calibration (ТЗ 7.4)
- [ ] RTT benchmark до co-located RPC
- [ ] Pre-buy checks latency under load
- [ ] Rent-exempt minimums через RPC
- [ ] Paper mode → 30 закрытых позиций → adaptive_sizing activation

---

## Сводка по приоритетам

| Приоритет | Этапы | Суммарно |
|---|---|---|
| 🔴 P0 (без этого ничего не работает) | 1, 2, 3, 4, 5 | ~8-10 недель |
| 🟡 P1 (работает, но неполноценно) | 6, 7, 8 | ~3 недели |
| 🟢 P2 (polish до прода) | 9, 10, 11 | ~3-4 недели |
| **Итого** | | **~14-17 недель** |

## Открытые вопросы

> [!IMPORTANT]
> 1. **Geyser provider**: Планируется использовать Yellowstone gRPC, Helius, или собственный валидатор с Geyser плагином?
> 2. **Vault/KMS**: HashiCorp Vault, AWS KMS, или encrypted file on disk?
> 3. **Jito SDK**: Использовать официальный `jito-sdk-rust` или самописный HTTP-клиент?
> 4. **Devnet testing**: Нужен ли отдельный devnet-профиль конфигурации?
> 5. **Paper mode**: Реализовывать как отдельный route (без реальной отправки) или как полный mock Solana?
> 6. **Solana SDK version**: Текущий `solana-client v1.18.26` имеет future-incompat warnings — обновлять до Agave SDK?



ЗАМЕТКИ НА ПОТОМ:
Этап 1: Единственное, что по-прежнему остаётся честной пометкой "не до конца готово" из первоначального аудита — load_keypair() использует SOLBOT_KEYPAIR_JSON (dev-режим), а не реальный Vault/KMS API. Это не блокер для Этапа 1 как такового (в плане это и было явно помечено как "или encrypted file + env" — рабочий вариант для промежуточного состояния), но для боевого прода это остаётся открытым пунктом, как и было отмечено.

подтверждено лучше, чем ожидалось: max_signal_age_slots реализован дважды — в отдельном модуле signal_filter.rs (судя по doc-комментарию //! 1. max_signal_age_slots (e.g. 30 slots) — ignores signals that arrived too late) и в signal_validator.rs с явным SignalValidatorConfig { max_signal_age_slots: u64 }, дефолтным значением MAX_SIGNAL_AGE_SLOTS (наверняка = 30, как в плане) и даже есть unit-тест assert_eq!(validator.config.max_signal_age_slots, 30). Это реальная, протестированная реализация, а не мой самодельный костыль в debug-бинаре.

Финальная версия заметки — готова к вставке
markdown
ЗАМЕТКИ НА ПОТОМ (Этап 2, DEX Listeners) — ФИНАЛЬНАЯ версия после полной проверки:

1. КРИТИЧНО (единственный настоящий блокер логики) — geyser_client.rs::
   subscribe_to_programs() игнорирует параметр program_ids на уровне 
   построения SubscribeRequest. Комментарий в коде: "TEMPORARY: Remove 
   filter to test if stream works at all" — SubscribeRequestFilterTransactions 
   строится без account_include, подписка ловит ВСЕ транзакции сети без 
   фильтра. Нарушает прямое требование исходного плана ("иначе лимит 
   кредитов сгорит за час"). Важно: сами вызывающие (RaydiumListener, 
   MeteoraListener, PumpFunListener в dex_listeners/) передают program_ids 
   ПРАВИЛЬНО — баг локализован строго в geyser_client.rs, чинить нужно 
   только его.

2. КРИТИЧНО — обработка UpdateOneof::Transaction в geyser_client.rs создаёt 
   фиктивный AccountUpdate (owner: Pubkey::default(), data: vec![], 
   account: Pubkey::new_unique() как placeholder). Даже при рабочем 
   фильтре эта ветка не даст dex_listeners реальных данных для парсинга. 
   Правильный путь — скорее всего вообще не нужна transaction-ветка: 
   dex_listeners ожидают Account-based обновления (судя по 
   parse_raydium_account(update: AccountUpdate)), а не Transaction-based. 
   Нужно проверить/переключить SubscribeRequest на accounts-фильтр 
   (owner = program_id) вместо transactions-фильтра.

3. ХОРОШАЯ НОВОСТЬ — сигнальная логика написана содержательно и БЕЗ 
   mock/dummy/placeholder:
   - dex_listeners/{raydium,meteora,pump_fun}.rs — реальные, используемые 
     в main.rs (main.rs:5 импортирует все три). Собирают program_ids 
     корректно (Raydium: AMM v4 + CP-MM + CLMM все три сразу в одном 
     subscribe_to_pool_creation).
   - max_signal_age_slots РЕАЛИЗОВАН и протестирован дважды: 
     signal/signal_validator.rs (SignalValidatorConfig.max_signal_age_slots, 
     дефолт = 30, есть unit-тест assert_eq!) и отдельно упомянут в 
     signal/signal_filter.rs. validate_signal() вызывается в каждом из 
     трёх dex_listeners с логом "Signal filtered by validator (too old)" 
     при отбраковке.
   - Всё это заблокировано ИСКЛЮЧИТЕЛЬНО багом #1/#2 в geyser_client.rs — 
     сигнальный pipeline ниже по потоку готов принимать данные, просто 
     сейчас до него ничего не доходит.

4. БЛОКЕР ИНФРАСТРУКТУРЫ — используемый Helius API-ключ на Free-плане. 
   Free-план не даёт доступа к Geyser gRPC/LaserStream (обычный JSON-RPC 
   работает — getHealth возвращает ok). TCP/TLS/HTTP2-хендшейк к gRPC 
   endpoint проходит успешно (соединение открывается на транспортном 
   уровне), но реальные subscribe_update события не приходят вообще 
   (0 за 30-120 сек прослушки на 6 самых активных mainnet DEX-программах — 
   физически исключает версию "просто мало активности"). Открытый вопрос 
   из исходного плана ("Yellowstone gRPC, Helius, или свой валидатор?") 
   так и не решён — нужно либо апгрейдить тариф Helius, либо сменить 
   провайдера.

5. Инструмент для повторной проверки после фиксов: 
   crates/solbot-strategy/src/bin/dex_listener_debug.rs — самописный 
   debug-бинарь (не часть боевого кода), слушает все 6 DEX программ live 
   напрямую через geyser_client, парсит через from_account_data() парсеры, 
   проверяет slot age самостоятельно (дублирует логику SignalValidator, 
   так как звал geyser_client напрямую, а не через dex_listeners). 
   ПОСЛЕ починки geyser_client.rs — лучше тестировать через реальные 
   RaydiumListener::subscribe_to_pool_creation() / 
   PumpFunListener::subscribe_to_bonding_curve() / 
   MeteoraListener::subscribe_to_pool_creation(), а не через мой debug-бинарь, 
   раз боевой код уже написан правильно и использует SignalValidator.

ИТОГОВЫЙ ВЫВОД: реальная готовность Этапа 2 выше первоначальной оценки. 
Вся сигнальная логика (парсинг, дедупликация по времени, slot-age валидация) 
написана полноценно, протестирована юнит-тестами, без заглушек. Единственная 
точка отказа — один общий баг в geyser_client.rs (отсутствие account_include 
фильтра при построении SubscribeRequest + бесполезная Transaction-ветка вместо 
нужной Account-ветки), который блокирует ВСЕ три DEX-листенера разом, плюс 
отдельный инфраструктурный блокер (тариф Helius). Оценка трудозатрат на 
исправление geyser_client.rs — вероятно от нескольких часов до 1-2 дней 
(нужно понять актуальный API yellowstone_grpc_proto для account_include 
фильтра), а не недели, как можно было бы предположить по общей оценке 
"~1.5-2 недели" в исходном плане Этапа 2.

СЛЕДУЮЩИЙ ШАГ (Этап 2):

1. [В РАБОТЕ] Дан промпт агенту на исправление geyser_client.rs:: 
   subscribe_to_programs() — переключение с transactions-based на 
   accounts-based subscription с owner-фильтром по program_ids, 
   удаление TEMPORARY-хака и фиктивного AccountUpdate в ветке 
   Transaction. См. diff после выполнения.

2. [ЗАВТРА] Нужно сменить Geyser-провайдера — текущий Helius Free-план 
   не даёт доступа к gRPC-стриму (подтверждено: 0 событий за 120 сек 
   на 6 активных DEX-программах, при этом обычный JSON-RPC работает). 
   Изученные варианты (все платные, бесплатного полноценного Geyser 
   практически не существует на рынке):
   - Chainstack — от $49/мес
   - AllenHark — от $99/мес (Yellowstone gRPC), + staked RPC $49/мес
   - Shyft — от $199/мес, но unlimited без метеринга по кредитам
   - Helius LaserStream — нужно уточнить минимальный платный тариф
   - QuickNode — добавка на Build/Accelerate планах, входит в Scale/Business
   Стоит проверить, есть ли у кого-то free trial на несколько дней — 
   этого хватит, чтобы прогнать dex_listener_debug.rs и подтвердить 
   фикс перед принятием решения о постоянном провайдере.

3. [АЛЬТЕРНАТИВА, бесплатно уже сейчас] Пока провайдер не выбран — 
   можно временно проверить логику парсинга через публичный бесплатный 
   WebSocket (wss://api.mainnet-beta.solana.com), не требующий ключа. 
   В проекте уже есть рабочий WsListener (использовался в 
   stage_1_2_check.rs), просто не подключён к dex_listeners. Это не 
   заменит Geyser для прода (медленнее, менее надёжно — поэтому план 
   изначально предпочитает Geyser), но подтвердит корректность 
   SignalValidator/dedup/парсеров на живых mainnet-данных без затрат.

4. После смены/подтверждения провайдера — обновить GEYSER_ENDPOINT 
   и GEYSER_X_TOKEN на новые значения и повторить live-тест через 
   dex_listener_debug.rs (или напрямую через RaydiumListener/
   MeteoraListener/PumpFunListener, раз они уже написаны правильно).