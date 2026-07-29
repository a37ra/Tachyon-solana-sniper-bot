# Калибровка таймингов и риск-параметров

В соответствии с ТЗ (раздел 7.4), перед запуском бота в `live` режим, необходимо выполнить калибровку на целевом сервере (co-located с Jito Block Engine).

## Шаг 1. Замер RTT до целевого RPC

Выполните скрипт `scripts/calibrate_rtt.sh`, чтобы получить p50 и p99 задержку `simulateTransaction` и `sendTransaction` до вашего Primary RPC.

Целевые показатели:
- `simulateTransaction` p50: ~10-20ms
- `sendTransaction` p50: ~5-15ms

Если ваши значения значительно выше, проверьте сетевую близость сервера к RPC ноде.

## Шаг 2. Бенчмарк цикла pre-buy checks

Запустите `scripts/benchmark_pre_buy_checks.sh`. Этот скрипт выполнит серию параллельных запросов (freeze authority, LP lock, extensions, simulate_sell) и выведет p50 и p99 для всей группы.

Убедитесь, что:
- P99 не превышает `hard_timeout_ms` (150ms).
- Доминантной по времени проверкой является `simulate_sell`.

## Шаг 3. Получение актуального rent-exempt минимума

Solana периодически пересматривает требования к rent-exemption. Не полагайтесь на хардкод.
Запустите `scripts/get_rent_exempt_minimums.sh`.

Скрипт вернет:
- ATA Rent (обычно ~0.00203928 SOL)
- Durable Nonce Account Rent (размер 80 байт, обычно ~0.0015 SOL)

Обновите значения в `config/tiers/*.yaml` (секция `hot_wallet.calculation`), чтобы `hot_wallet_cap_sol` был рассчитан правильно.

## Шаг 4. Переход из Paper в Live режим

Adaptive Sizing (Kelly Criterion) требует статистической значимости. До тех пор, пока бот не закроет `min_sample_size` (по умолчанию 30) сделок, он использует статический `base_size_pct` из настроек тира.

Рекомендуется:
1. Включить `paper_trading: enabled: true` в `base.yaml`.
2. Накопить ≥30 закрытых виртуальных позиций.
3. Проанализировать рассчитанные `p` (win rate) и `b` (payoff ratio).
4. Если результаты удовлетворительны, отключить paper trading и перевести бота в live.

## Шаг 5. Фиксация результатов

После завершения всех шагов, сохраните метрики калибровки в внутреннем журнале вашей команды и снимите флаг (или просто начните работу), подтвердив, что `timing_constants_not_benchmarked` больше не актуален.
