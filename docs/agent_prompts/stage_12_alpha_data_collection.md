# Agent Prompt — Этап 12: Alpha Data Collection (bundle / funder / concentration)

## Why this stage exists (critical context)

Technical risk_gate (stages 1–5) only answers **“can we sell this token?”**.  
Paper ACCEPTs were still classic **dev pump-and-dump** (DB→SB→DS in 30–90s): fully sellable, economically toxic.

**Alpha Layer (TZ §8)** is **Layer 0** — separate gate. This stage builds **raw features only** (no hard_block decisions yet — that is stage 14). Always **persist snapshots** including cases that will later hard_block (training data).

## Read before coding

| Doc | What |
|-----|------|
| `tz-solana-sniper-bot-production.txt` §8.1–8.2, §8.7–8.8 | Spec |
| `docs/alpha/ALPHA_LAYER_FORMULAS.md` §1 | Exact formulas |
| `config/alpha_layer.yaml` | Defaults |
| `migrations/0004_alpha_layer.sql` | DDL (apply if not applied) |
| `solana-sniper-bot-file-tree.txt` → `alpha/`, `state/*alpha*` | Paths |
| Existing `signal/create_ix_source.rs`, `signal/logs_parser.rs` | Create events + slots |
| `risk_gate/*` | **Do not weaken**; alpha runs beside/before it later |

## Goal

Implement:

1. `crates/solbot-strategy/src/alpha/mod.rs` — module skeleton, public types for feature snapshot
2. `bundle_detector.rs` — `bundler_pct(t)`
3. `funder_graph.rs` — `funder_cluster_pct`, `fresh_wallet_ratio` (2-hop, **cache funders**)
4. `concentration.rs` — `top10_holder_pct`, `CM = top10 * N / 10` only if `N >= min_buyers_for_concentration_check`
5. `state/alpha_signals_repo.rs` — insert rows into `alpha_signals` (decision may be NULL/pending this stage)
6. Unit tests with **synthetic** buyers/slots/bundles (no mainnet required for CI)

## Feature definitions (do not invent alternatives)

```
bundler_pct = sum(buy_amount in same slot OR same jito bundle as create_tx)
            / sum(all buy_amount in window)

funder(w) = first inbound SOL transfer wallet before w's first outbound (depth ≤2)
funder_cluster_pct = max_cluster_volume / total_buy_volume_in_window
fresh_wallet_ratio = buyers_with_first_tx_age < threshold_min / all_buyers

CM = top10_holder_pct / (10/N)   # N = unique buyers; NULL if N < min_buyers
# Exclude LP/burn/program accounts from top10; track dev_buy_pct separately if available
```

Window: `alpha_window_ms` from config (default 4000).

## Constraints

- **No paid third-party alpha APIs** (Birdeye/GMGN API). RPC + existing Geyser/WS only.
- Cache aggressively: `getSignaturesForAddress` per buyer wallet is expensive — TTL cache by pubkey.
- Never block the hot path for unbounded RPC: budget timeouts; on timeout leave feature NULL and still record row.
- Do **not** implement hard_block or probe_scale here (stages 13–14).
- Do **not** change create-ix detection or fee_recipient Global load.
- Migration number is **0004** (0002/0003 already used).

## Integration (minimal this stage)

- Hook: after create-ix Signal is formed (or parallel task), spawn/collect features for that mint/pool/slot.
- Even if pipeline still buys on risk_gate only, **write alpha_signals** for every considered mint (accept and reject paths if easy; at least all ACCEPT candidates).

## Tests (required)

- Synthetic: 3 wallets same slot as create → high bundler_pct
- Synthetic: 5 wallets same funder → high funder_cluster_pct
- Synthetic: N=12 equal buys → CM ≈ 1; skewed top2 → CM >> 1
- N < min_buyers → CM is None

## Verification

```bash
# apply migration if your tooling needs it
cargo test -p solbot-strategy --lib alpha
rg -n "bundler_pct|funder_cluster|concentration_multiplier" crates/solbot-strategy/src/alpha
```

## Done when

- [ ] Modules compile and are wired in `alpha/mod.rs` + `lib`/`main` module tree
- [ ] Repo inserts work
- [ ] Unit tests green
- [ ] `implementation_plan.md` stage 12 checkboxes updated
- [ ] Short note in `notes.md`: how to enable feature collection in paper

## Anti-patterns (agents fail these)

- Flat threshold on top10 without CM
- Calling RPC per tick without cache
- Mixing alpha metrics into `risk_gate_latency_ms`
- Hardcoding fee_recipient or program IDs incorrectly
- Marking stage complete without persisting rejected/blocked candidates
