# Временный readme с командами и полезными бинарниками

## Пока готов только P0 из плана, всего P3

Быстрая инструкция по сборке и проверке
1. Перейти в корень проекта
```
cd /home/astra/Проекты/solana-bot
```
2. Проверка кода
```
cargo check --package solbot-strategy
```

3. Сборка релиза
```
cargo build --release --package solbot-strategy
```

4. Запуск тестов
```
cargo test --package solbot-strategy
```

Если нужно только конкретный модуль:
```
cargo test --package solbot-strategy -- risk_gate
```
### Команды для папир-режима
1. Запуск в paper mode
```
SIGNAL_SOURCE=helius_ws cargo run --release --package solbot-strategy --bin solbot-strategy -- --paper
```

2. Если используешь Devnet и хочешь гарантированно безопасно
```
SOLBOT_RPC_URL=https://api.devnet.solana.com \
SIGNAL_SOURCE=helius_ws \
SOLBOT_PAPER_TRADING=true \
cargo run --release --package solbot-strategy --bin solbot-strategy -- --paper
```

3. Альтернативный вариант с загрузкой .env
Если у тебя есть файл .env в корне, достаточно:

```
cargo run --release --package solbot-strategy --bin solbot-strategy -- --paper
```

Полезные переменные окружения
```
export SOLBOT_PAPER_TRADING=true
export SIGNAL_SOURCE=helius_ws
export SOLBOT_RPC_URL=https://api.mainnet-beta.solana.com
export SOLBOT_SIGNER_SOCKET=/tmp/solbot-signer.sock
export SOLBOT_KEYPAIR_JSON='{"path":"/path/to/keypair.json"}'
export GEYSER_ENDPOINT="https://laserstream.helius-rpc.com:443"
export GEYSER_X_TOKEN="твой_helius_key"
```

Полезные бинарники в solbot-strategy

solbot-strategy — основной бот
solbot-benchmark — нагрузочное / бенчмарк-приложение
stage_1_2_check — проверка стадии 1/2
devnet_roundtrip — devnet-тесты

Пример запуска бенчмарка:
```
cargo run --release --package solbot-strategy --bin solbot-benchmark
```

### Проверка состояния перед запуском
1. Убедиться, что ключи доступны
```
ls -l /path/to/keypair.json
```

2. Проверить .env
```
cat .env
```

3. Быстрая сборка + запуск paper
```
cargo build --release --package solbot-strategy && \
SIGNAL_SOURCE=helius_ws cargo run --release --package solbot-strategy --bin solbot-strategy -- --paper
```

Что смотреть в логах
NEW POOL DETECTED — бот принял новый сигнал
TOTAL CODE LATENCY — время обработки сигнала в коде
RPC Simulation RTT — задержка симуляции
Inbound Stream Age — возраст сигнала в потоке
Pre-buy checks timed out — если Risk Gate слишком медленно отвечает

Команда для живого режима (когда будешь готов)
```
export SOLBOT_PAPER_TRADING=false
export SOLBOT_RPC_URL=https://api.mainnet-beta.solana.com
export SOLBOT_KEYPAIR_JSON='{"path":"/path/to/keypair.json"}'
export SOLBOT_SIGNER_SOCKET=/tmp/solbot-signer.sock
export SIGNAL_SOURCE=helius_ws

cargo run --release --package solbot-strategy --bin solbot-strategy
```

Быстрый чек: работает ли бумажный режим
```
SIGNAL_SOURCE=helius_ws cargo run --release --package solbot-strategy --bin solbot-strategy -- --paper
```

Если запустится и будут появляться строки вида NEW POOL DETECTED, значит всё в порядке.