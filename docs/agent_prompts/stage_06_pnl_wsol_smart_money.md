# Agent Prompt — Этап 6: PnL Net, WSOL unwrap, Smart Money

## Context (read first)

You are implementing **stage 6** of solana-sniper-bot. Stages 1–5 (signal create-ix, risk_gate, builders, routing, actor paper buy) are largely done. Do **not** rewrite alpha layer (stages 12–15) or risk_gate classification unless required.

**Sources of truth:**
- TZ: `tz-solana-sniper-bot-production.txt` §4.5 (PnL), §4.7 (WSOL), §2.5 (smart money)
- Plan: `implementation_plan.md` → Этап 6
- Tree: `solana-sniper-bot-file-tree.txt` → `pnl/`, `wsol/`, `smart_money/`

## Goal

1. **PnL net per position:** track priority fees, Jito tips, ATA rent open/close; write `realized_pnl_net_sol`.
2. **WSOL:** auto-unwrap when WSOL ATA balance > threshold; `CloseAccount` rent reclaim.
3. **Smart money:** load watchlist from DB; emit `smart_money_hit`; modes `tag_only` (default) / `soft_boost` / `hard_gate` without weakening risk_gate hard checks.

## Constraints

- Never put secrets in logs.
- `hard_gate` must stay off until `min_sample_size` closed positions (TZ 2.5) — default `tag_only`.
- soft_boost may only derate/boost size inside existing `size_max_pct` clamp — never skip freeze/LP/simulate_sell.
- Prefer extending existing modules over new crates.

## Deliverables

- [ ] `pnl/mod.rs` (or expand): fee/tip/rent fields on close → SQLite
- [ ] `wsol/mod.rs`: unwrap + close empty ATA
- [ ] `smart_money/*`: subscription hooks + mode wiring in pre-buy path
- [ ] Unit tests for PnL arithmetic and mode gating
- [ ] Update `implementation_plan.md` checkboxes for stage 6

## Do NOT

- Implement alpha bundler/dev_history (stage 12–13)
- Change SIGNAL_SOURCE defaults
- Weaken BuilderError / honeypot classifier

## Verification

```bash
cargo test -p solbot-strategy --lib pnl wsol smart_money
# paper run optional: ensure no regression in PIPELINE STATS
```

## Hints for the agent

- Grep existing `realized_pnl`, `priority_fee`, `jito` tip tracking before inventing schemas.
- Watchlist table already in migrations; seed under `data/wallet_watchlist/`.
- Paper mode: smart_money can log-only without buy path changes if mode=tag_only.
