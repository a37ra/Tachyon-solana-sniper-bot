# Agent Prompt — Этап 9: CLI Live Ops

## Context

`solbot-cli` crate has command skeleton. Wire against running strategy/signer for ops. TZ §6 CLI list.

## Goal

Working commands:

- `kill-switch [on|off]`
- `position [list|show <id>|force-close <id>]`
- `circuit-breaker [status|reset]`
- `mode [paper|live]`
- `wallet [balance|rotate]` (rotate may be stub with clear error if KMS missing)

## Constraints

- CLI must not load hot private key into strategy process memory.
- force-close goes through escalation path, not ad-hoc raw transfer.
- Auth: local UDS or admin token — no open TCP without auth.

## Deliverables

- [ ] Commands hit live control plane (UDS/HTTP admin as designed)
- [ ] Integration test with mock control server
- [ ] Plan checkboxes

## Verification

```bash
cargo test -p solbot-cli
cargo run -p solbot-cli -- --help
```

## Hints

- Mirror Telegram admin_commands where possible.
- Prefer JSON output flag for scripting.
