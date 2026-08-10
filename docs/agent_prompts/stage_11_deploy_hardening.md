# Agent Prompt — Этап 11: Deploy & Hardening

## Context

`deploy/` has systemd skeletons, litestream, vault policy stubs. Fill for real ops. TZ §6, §7.4.

## Goal

1. Complete systemd units: strategy, signer (strictest), telegram, notify
2. Vault-agent or documented encrypted-file prod path
3. Litestream continuous WAL replicate + restore drill documented
4. sysctl/BBR notes for co-lo host
5. Calibration runbook: RTT simulateTransaction, pre-buy under load, paper ≥30 closes before adaptive_sizing

## Constraints

- Signer unit: ProtectSystem=strict, MemoryDenyWriteExecute, LimitCORE=0, no leftover world-readable key files
- Never commit real secrets; only `.env.example` keys
- RTO target <10s hydration (KPI 1.3)

## Deliverables

- [ ] Filled unit files under `deploy/systemd/`
- [ ] `docs/RUNBOOK.md` restore + calibration sections
- [ ] `docs/CALIBRATION.md` checklist items executable
- [ ] Plan checkboxes

## Verification

- Dry-run: `systemd-analyze verify deploy/systemd/*.service`
- Document manual litestream restore steps (can be script under `scripts/`)

## Hints

- Align with existing `deploy/` tree; don't invent parallel layout.
- Link calibration to flag `timing_constants_not_benchmarked` if present in code.
