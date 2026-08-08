-- 0003_fix_closed_at_utc_nullable.sql
-- Fix positions.closed_at_utc semantics after 0002_align_with_tz_v1.sql by making it nullable and preserving closed-only timestamps.

ALTER TABLE positions RENAME TO positions_old;

ALTER TABLE state_transitions RENAME TO state_transitions_old;

CREATE TABLE positions (
    position_id TEXT PRIMARY KEY,
    mint TEXT NOT NULL,
    pool_address TEXT NOT NULL,
    dex_program_id TEXT NOT NULL DEFAULT '',
    tier TEXT NOT NULL,
    status TEXT NOT NULL,
    size_sol REAL NOT NULL,
    entry_price_sol REAL NOT NULL,
    size_pct_of_capital REAL NOT NULL DEFAULT 0.0,
    remaining_token_pct REAL NOT NULL DEFAULT 1.0,
    hard_sl_price_sol REAL NOT NULL DEFAULT 0.0,
    trailing_active INTEGER NOT NULL DEFAULT 0,
    trailing_high_water_sol REAL,
    realized_pnl_net_sol REAL DEFAULT 0.0,
    opened_at_utc INTEGER NOT NULL,
    closed_at_utc INTEGER
);

CREATE INDEX IF NOT EXISTS idx_positions_status ON positions(status);
CREATE INDEX IF NOT EXISTS idx_positions_mint ON positions(mint);

INSERT INTO positions (
    position_id,
    mint,
    pool_address,
    dex_program_id,
    tier,
    status,
    size_sol,
    entry_price_sol,
    size_pct_of_capital,
    remaining_token_pct,
    hard_sl_price_sol,
    trailing_active,
    trailing_high_water_sol,
    realized_pnl_net_sol,
    opened_at_utc,
    closed_at_utc
)
SELECT
    position_id,
    mint,
    pool_address,
    dex_program_id,
    tier,
    status,
    size_sol,
    entry_price_sol,
    size_pct_of_capital,
    remaining_token_pct,
    hard_sl_price_sol,
    trailing_active,
    trailing_high_water_sol,
    realized_pnl_net_sol,
    opened_at_utc,
    CASE WHEN status = 'closed' THEN closed_at_utc ELSE NULL END
FROM positions_old;

CREATE TABLE state_transitions (
    id TEXT PRIMARY KEY,
    position_id TEXT NOT NULL REFERENCES positions(position_id),
    from_status TEXT NOT NULL,
    to_status TEXT NOT NULL,
    reason TEXT NOT NULL,
    tx_signature TEXT,
    created_at_utc INTEGER NOT NULL
);

INSERT INTO state_transitions (
    id,
    position_id,
    from_status,
    to_status,
    reason,
    tx_signature,
    created_at_utc
)
SELECT
    id,
    position_id,
    from_status,
    to_status,
    reason,
    tx_signature,
    created_at_utc
FROM state_transitions_old;

DROP TABLE state_transitions_old;
DROP TABLE positions_old;
