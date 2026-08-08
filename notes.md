# 🚀 HFT Infrastructure & Bottleneck TODO

> **Текущий статус:** Переведено в режим экономии RPS из-за лимитов бесплатного/публичного RPC (`api.mainnet-beta.solana.com`).

---

## ⚠️ ВРЕМЕННЫЕ ОГРАНИЧЕНИЯ (Сделано под Public RPC)

- [x] **Semaphore уменьшен до 5** (было: 30)
  - *Причина:* Public Solana RPC режет трафик при >100 req / 10 sec. Семафор на 30 выдавал ~600 QPS и вызывал таймауты до 450+ мс.

- [x] **Load Shedding включен** (отсечение старых сигналов)
  - Сигналы старше 300ms дропаются перед RPC-запросами для экономии квоты.

- [x] **Переиспользование RpcClient** (Connection Pooling)
  - `simulate_sell::check` теперь принимает `Arc<RpcClient>` вместо создания нового соединения на каждый сигнал.
  - Это включает HTTP Keep-Alive и срезает 30-60ms на TCP+TLS handshake.
  - `pre_buy_checks_timeout_ms` увеличен до 3000ms для учёта simulate_sell RPC latency с connection pooling (Anti-Rug Filters занимают ~500ms).
  - Файлы: `crates/solbot-strategy/src/risk_gate/simulate_sell.rs`, `crates/solbot-strategy/src/risk_gate/mod.rs`, `crates/solbot-strategy/src/main.rs`

---

## 🛠 ЧЕК-ЛИСТ ПРИ ПЕРЕХОДЕ НА PRIVATE RPC / GEYSER (Helius / Triton / Shyft)

Как только подключаем платный endpoint (или Dedicated Node) — **ОБЯЗАТЕЛЬНО выполнить следующие шаги**:

### 1. Масштабирование параллелизма
- [ ] **Вернуть `Semaphore` на 30** (или выше, в зависимости от лимита RPS на приватном тарифе).
  - Файл: `crates/solbot-strategy/src/main.rs`, строка 194
- [ ] Увеличить размер `mpsc`-каналов очереди сигналов под высокую пропускную способность.

### 2. Защита очереди (Load Shedding / Signal Drop)
- [ ] **Добавить отсечение устаревших сигналов (TTL Check):**
  Дропать входящий сигнал в самом начале `run_pre_buy_checks`, если `inbound_age_ms > 500ms`.
  ```rust
  if signal.inbound_age_ms > 500.0 {
      tracing::warn!("Signal stale ({}ms), dropping", signal.inbound_age_ms);
      return RiskGateVerdict::Reject { reason: "Stale signal".into() };
  }
  ```
  - Файл: `crates/solbot-strategy/src/risk_gate/mod.rs`, функция `run_pre_buy_checks`

### 3. Оптимизация Risk Gate & Simulate Sell

- [ ] **Исправить from_ws_update:** Гарантировать, что Mint и Pool аккаунты парсятся напрямую из байт входящего gRPC/WS события, минуя PreBuyCheckContext::fetch (убираем лишний 1 RTT).
  - Файл: `crates/solbot-strategy/src/risk_gate/mod.rs`, функция `from_ws_update`

- [ ] **Оставить simulate_sell**, но пустить его по отдельному высокоскоростному приватному каналу/RPC connection-пулу.
  - Файл: `crates/solbot-strategy/src/risk_gate/simulate_sell.rs`

- [ ] **Уменьшить PRE_BUY_CHECK_RPC_TIMEOUT_MS с 300ms обратно до 80-100ms** (после переезда бота на VPS во Франкфурте/Амстердаме).
  - Файл: `crates/solbot-strategy/src/risk_gate/mod.rs`, строка 17

---

## 📊 Целевые метрики после переезда

- **Inbound Stream Age:** < 50 ms
- **Anti-Rug Filters Latency:** < 50 ms (включая simulate_sell)
- **Total Code Latency:** < 60 ms
