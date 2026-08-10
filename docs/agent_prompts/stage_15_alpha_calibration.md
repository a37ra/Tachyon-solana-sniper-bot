# Agent Prompt — Этап 15: Alpha Layer Calibration

## Context

Stages 12–14 must be running in paper (or limited live) long enough to fill:

- `alpha_signals` with features + `decision`
- `outcome_label` via labeling_worker (`dump` / `not_dump`)

This stage **fits weights and thresholds** — does not invent new features.

**Read:** TZ §8.4, §8.5.3, Appendix C; `docs/alpha/ALPHA_LAYER_FORMULAS.md` §5–6; `docs/CALIBRATION.md`.

## Goal

1. Export dataset: features + outcome_label (drop pending)
2. Target sample: **≥150–200 per class** (or document shortfall and do not clear calibration flag)
3. Fit `w1..w5` (logistic regression or grid search on precision/recall held-out)
4. Measure empirical `δ` = mean adverse drift over `probe_window_sec` on soft-gate-clean non-dump tokens
5. Recompute `p* = δ / (L + δ)` with tier `effective_loss_pct`
6. Measure hard-block false positive rate (labeled not_dump but hard_blocked) → propose new `hard_max_pct`
7. Write calibrated values into `config/alpha_layer.yaml` (or `config/alpha_layer.calibrated.yaml`) with timestamp + sample sizes
8. Clear code flag `alpha_weights_not_calibrated` if one exists (mirror `timing_constants_not_benchmarked`)

## Constraints

- **No silent overwrite** of defaults without backup of previous yaml.
- Do not enable `hard_gate` smart_money as substitute for alpha calibration.
- Report metrics: AUC/precision/recall, FP hard-block rate, suggested p*, δ.
- If sample too small: leave defaults, set flag still true, write report only.

## Deliverables

- [ ] Script under `scripts/calibrate_alpha.py` or Rust bin `alpha_calibrate`
- [ ] Report markdown under `docs/alpha/CALIBRATION_REPORT_YYYYMMDD.md`
- [ ] Updated config + plan checkboxes
- [ ] notes.md pointer

## Verification

- Dry-run on synthetic CSV of alpha_signals
- Re-run paper 30–60 min: hard_block rate and ACCEPT quality note (manual)

## Anti-patterns

- Fitting on <30 samples and declaring production-ready
- Optimizing only training accuracy (must held-out)
- Changing label rule 90s without versioning labels
