# Alpha Layer — формулы и численные выкладки

Источник: ТЗ §8. Не калиброванные константы — framework (см. Приложение C ТЗ).

---

## 1. On-chain сигналы

### 1.1 `dev_buy_pct`

```
dev_buy_pct = dev_wallet_tokens_bought / total_supply
```

### 1.2 `bundler_pct(t)`

```
bundler_pct(t) =
  Σ(amount | buy landed in same Jito bundle OR same slot as pool-creation tx)
  / Σ(amount | all buys up to t)
```

### 1.3 Funder graph

Для покупателя `w` в окне `alpha_window_ms`:

- `funder(w)` = кошелёк первого входящего SOL-transfer до первой исходящей tx `w` (глубина 2 hop).
- Кластер = множество покупателей с общим funder (≤2 hops).

```
funder_cluster_pct =
  max_cluster( Σ buy_amount в кластере ) / Σ buy_amount всех покупателей в окне

fresh_wallet_ratio =
  count(buyers | age_first_tx < wallet_age_fresh_threshold_min)
  / count(all buyers в окне)
```

### 1.4 Concentration multiplier (CM)

При `N ≥ min_buyers_for_concentration_check` (default 25):

```
# нулевая гипотеза: i.i.d. равные покупки → E[top10] ≈ 10/N  (N > 10)
CM = top10_holder_pct / (10 / N)
   = top10_holder_pct * N / 10
```

- `CM ≈ 1` — как при случайном распределении  
- `CM >> 1` — аномальная концентрация  
- `N < min_buyers` → CM не используется (NULL)

`top10_holder_pct` **исключает** LP, burn, known program accounts; dev-кошелёк учитывается отдельно в `dev_buy_pct`.

---

## 2. Dev History — Wilson lower bound (95%)

```
p̂ = k / n          # k = n_dumps, n = n_launches
z  = 1.95996       # Φ⁻¹(0.975)

center = p̂ + z²/(2n)
margin = z * sqrt( (p̂(1-p̂) + z²/(4n)) / n )
denom  = 1 + z²/n

wilson_lower_dump_prob = (center - margin) / denom
```

```
n == 0  → dev_history_score = NULL   (unknown_dev, neutral_prior в soft score)
n >= 1  → dev_history_score = wilson_lower_dump_prob
```

### Label rule (через `label_delay_min` после launch)

```
label = "dump"  IF within label_cliff_window_sec (90s) ANY:
  (a) dev_net_sell_pct >= label_dev_sell_pct (50%)
  (b) max_drawdown_pct from local ATH >= label_drawdown_pct (80%)
ELSE label = "not_dump"
```

---

## 3. Composite gate

### Hard blocks (any → reject)

```
dev_history_score  > block_threshold[tier]     # if not NULL
bundler_pct        > bundler.hard_max_pct[tier]
funder_cluster_pct > funder_cluster.hard_max_pct[tier]
```

### Soft score (only if no hard block)

```
norm(x) = clamp(x, 0, 1)   # or feature-specific normalization documented in code

alpha_risk_score =
    w1 * norm(bundler_pct)
  + w2 * norm(fresh_wallet_ratio)
  + w3 * norm(funder_cluster_pct)
  + w4 * norm(CM)                    # 0 if CM is NULL
  + w5 * (dev_history_score ?? neutral_prior)

# default weights (illustrative, equal): wi = 0.2
# neutral_prior default: 0.35 (neither clean nor dump)
```

### Size derate

```
size_multiplier = clamp(1 - alpha_risk_score, alpha_size_floor[tier], 1.0)
final_size_pct  = adaptive_sizing_output * size_multiplier
# never exceeds size_max_pct from §4.1
```

---

## 4. Probe-scale EV

```
S   = full target size (SOL) after §4.1 + §8.4
q   ∈ (0,1]  probe fraction;  probe_size = q·S,  confirm_size = (1-q)·S
p   = residual dump probability after soft-gate
δ   = expected adverse price drift over probe_window_sec for CLEAN token
L   = effective_loss_pct on emergency exit (§4.8)
```

```
EV_probe = (1-p)·S·E[r|clean] - (1-p)·(1-q)·S·δ - p·q·S·L
EV_full  = (1-p)·S·E[r|clean] - p·S·L

EV_probe - EV_full = (1-q)·S · [ p·L - (1-p)·δ ]
```

### Breakeven threshold

```
p* = δ / (L + δ)

# example: L=0.65, δ=0.05 → p* ≈ 0.077
```

### Decision rule

```
IF  alpha_risk_score "clean" AND p < p* AND skip_probe_if_clean_and_below_p_star:
      → direct_full
ELSE:
      → probe_scale with q = q_probe_default
```

Note: linear model has corner solution in `q` (sign of ΔEV independent of `q`); `q` is engineering (default 0.25), not EV optimum.

### Confirm / abort

- Dump confirmed in probe window → `emergency_exit_escalation` (4th trigger alongside hard SL / backfill / CB)
- Confirm buy → same `position_id`, VWAP `entry_price_sol`

---

## 5. Default config numbers (§8.6)

| Key | Default |
|-----|---------|
| alpha_window_ms | 4000 |
| probe_window_sec | 3 |
| min_buyers_for_concentration_check | 25 |
| concentration_multiplier_max | 3.0 |
| dev_history.block_threshold | 0.50 |
| dev_history.caution_low | 0.25 |
| label_cliff_window_sec | 90 |
| label_dev_sell_pct | 50 |
| label_drawdown_pct | 80 |
| label_delay_min | 60 |
| bundler.hard_max_pct | micro 70 / small 65 / medium 60 / large 55 |
| funder_cluster.hard_max_pct | micro 60 / small 55 / medium 50 / large 45 |
| wallet_age_fresh_threshold_min | 30 |
| weights | all 0.2 |
| alpha_size_floor | micro/small 0.3, medium 0.25, large 0.2 |
| q_probe_default | 0.25 |
| delta_drift_estimate_pct | 5 |
| neutral_prior | 0.35 |

---

## 6. Calibration targets (Этап 15)

- ≥150–200 labeled cases per class (`dump` / `not_dump`) before refitting `w1..w5`
- Empirical `δ` = mean adverse drift on soft-gate-clean tokens over `probe_window_sec`
- Recompute `p*` with measured `δ` and tier `effective_loss_pct`
- Measure hard-block false positive rate on paper; tune `hard_max_pct`
