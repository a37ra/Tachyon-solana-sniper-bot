-- 0004_alpha_layer.sql — ТЗ §8.7 Alpha Layer tables

CREATE TABLE IF NOT EXISTS dev_wallet_history (
    dev_wallet_address     TEXT PRIMARY KEY,
    n_launches             INTEGER NOT NULL DEFAULT 0,
    n_dumps                INTEGER NOT NULL DEFAULT 0,
    wilson_lower_dump_prob REAL NOT NULL DEFAULT 0,
    last_launch_mint       TEXT,
    last_labeled_at_utc    DATETIME,
    updated_at_utc         DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS dev_launch_labels (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    dev_wallet_address  TEXT NOT NULL REFERENCES dev_wallet_history(dev_wallet_address),
    mint                TEXT NOT NULL,
    launched_at_utc     DATETIME NOT NULL,
    label               TEXT NOT NULL CHECK (label IN ('dump', 'not_dump', 'pending')),
    dev_net_sell_pct    REAL,
    max_drawdown_pct    REAL,
    labeled_at_utc      DATETIME
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_dev_launch_labels_mint
    ON dev_launch_labels(mint);

CREATE TABLE IF NOT EXISTS alpha_signals (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    mint                    TEXT NOT NULL,
    pool_address            TEXT NOT NULL,
    snapshot_at_slot        INTEGER NOT NULL,
    dev_buy_pct             REAL,
    bundler_pct             REAL,
    fresh_wallet_ratio      REAL,
    funder_cluster_pct      REAL,
    unique_buyers_count     INTEGER,
    top10_holder_pct        REAL,
    concentration_multiplier REAL,
    dev_history_score       REAL,
    alpha_risk_score        REAL,
    decision                TEXT CHECK (decision IN ('hard_block', 'probe_scale', 'direct_full')),
    outcome_label           TEXT,
    created_at_utc          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_alpha_signals_mint ON alpha_signals(mint);
CREATE INDEX IF NOT EXISTS idx_alpha_signals_created ON alpha_signals(created_at_utc);
