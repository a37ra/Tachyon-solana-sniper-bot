<div align="center">
<img src="./bannertachyon.jpg" width="100%" alt="banner" />

# Solana Sniper Bot

**Автоматический execution-слой** для Solana: детекция create-пулов → technical risk-gate → build/sign → route (Jito/RPC).  
Paper-ready · multi-DEX · isolated signer

</div>

---

## Стек

| | |
|--|--|
| **Workspace** | `solbot-strategy` · `solbot-signer` · `solbot-cli` · `solbot-notify` · `solbot-telegram` · `solbot-common` |
| **Сигнал** | `logsSubscribe` create-ix (default) · optional `programSubscribe` / Geyser |
| **DEX** | Pump.fun · Raydium AMM/CPMM/CLMM · Meteora DLMM/DAMM v2 |
| **Risk-gate** | mint/freeze · LP lock · Token-2022 · simulate_sell (+ BuilderError) |

---

## Quick start

```bash
# 1. env
cp .env.example .env
# HELIUS_API_KEY=...
# SOLBOT_RPC_URL=https://mainnet.helius-rpc.com/?api-key=...
# SIGNAL_SOURCE=helius_logs
# SOLBOT_PAPER_TRADING=true

# 2. build
cargo build -p solbot-strategy --release

# 3. paper (mainnet read-only, no real txs)
cargo run -p solbot-strategy --release -- --paper
```

Смотри в логах: `create-ix signal` · `ACCEPT` / `REJECT` · `PIPELINE STATS` · `fee_recipient loaded`.

---

## Команды

```bash
# check / test
cargo check -p solbot-strategy
cargo test  -p solbot-strategy --lib
cargo test  -p solbot-strategy --lib risk_gate
cargo test  -p solbot-strategy --lib pump

# paper
cargo run -p solbot-strategy --release -- --paper

# e2e smoke (builders + risk path, no broadcast)
cargo run -p solbot-strategy --release --bin paper_pipeline_e2e

# latency / waterfall helper
cargo run -p solbot-strategy --release --bin solbot-benchmark
```

### Сигналы

| `SIGNAL_SOURCE` | Смысл |
|-----------------|--------|
| `helius_logs` | **default** — create-ix via logsSubscribe |
| `helius_ws` | programSubscribe (legacy; false “new pool” risk) |
| `both` / `helius_both` | logs + account |
| `geyser_grpc` | paid LaserStream / Yellowstone |

### Live (когда готово)

```bash
# 1) signer (отдельный процесс, ключ только там)
cargo run -p solbot-signer --release

# 2) strategy
export SOLBOT_PAPER_TRADING=false
export SOLBOT_SIGNER_SOCKET=/tmp/solbot-signer.sock
# private RPC + Jito required — public RPC fail-closed in live
cargo run -p solbot-strategy --release
```

---

## Env (минимум)

```bash
SIGNAL_SOURCE=helius_logs
HELIUS_API_KEY=
SOLBOT_RPC_URL=https://mainnet.helius-rpc.com/?api-key=
SOLBOT_PAPER_TRADING=true
SOLBOT_PUBLIC_RPC=true          # paper ok; live wants false + private
SOLBOT_PRE_BUY_TIMEOUT_MS=4000  # public; colo ~600–1500
SOLBOT_SIGNER_SOCKET=/tmp/solbot-signer.sock
```

Полный список — `.env.example`.

---

## Логи (что важно)

| Метка | Значение |
|-------|----------|
| `create-ix signal` | пойман create (mint/pool/dex) |
| `ACCEPT` / `REJECT` | risk-gate итог |
| `PIPELINE STATS` | accept/reject/buckets |
| `RPC Simulation RTT` | доминирует latency на public RPC |
| `builder_error` | сломанный ix layout (не honeypot) |
| `fee_recipient loaded` | pump Global fee_recipient с chain |

---

## Roadmap (кратко)

| Этапы | Фокус | Статус |
|-------|--------|--------|
| **1–5** | signal · risk-gate · builders · routing · actor paper | ✅ ~логика done |
| **6–11** | PnL · recovery · otel · CLI · chaos · deploy | ⬜ ops |
| **12–15** | **Alpha Layer** (dev/insider pre-filter + probe) | ⬜ spec only (ТЗ §8) |

> Technical risk-gate ≠ edge. Без Alpha live на сырых pump create = высокий риск DB→DS dump.

---

<div align="center">

## Готовность

```
██████████████░░░░░░░░░░░░░░░░░░░░ ~42% до прод
```

| Слой | % | |
|------|---|--|
| P0 pipeline (1–5) technical | **88%** | paper ACCEPT stable |
| Live / private RPC / Jito ops | **35%** | needs Business+FRA+signer e2e |
| Alpha Layer (12–15) | **5%** | TZ + prompts, no code |
| Prod ops (6–11) | **25%** | stubs / partial |
| **Overall (earn-ready sniper)** | **~42%** | |
| **Overall (safe executor)** | **~72%** | |

<sub>обновлено 2026-08-10 · paper-first · не financial advice</sub>

</div>
