-- Включение Write-Ahead Logging (WAL) для конкурентного доступа (требование Litestream)
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA foreign_keys=ON;

-- Таблица: positions (Учет позиций)
CREATE TABLE IF NOT EXISTS positions (
    id TEXT PRIMARY KEY,
    mint TEXT NOT NULL,
    pool_address TEXT NOT NULL,
    tier TEXT NOT NULL, -- micro, small, medium, large
    status TEXT NOT NULL, -- opening, open, closing, closed, stuck_manual_intervention_required
    size_sol REAL NOT NULL,
    entry_price_sol REAL NOT NULL,
    realized_pnl_sol REAL DEFAULT 0.0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_positions_status ON positions(status);
CREATE INDEX IF NOT EXISTS idx_positions_mint ON positions(mint);

-- Таблица: state_transitions (Аудит смены статусов позиций)
CREATE TABLE IF NOT EXISTS state_transitions (
    id TEXT PRIMARY KEY,
    position_id TEXT NOT NULL REFERENCES positions(id),
    from_status TEXT NOT NULL,
    to_status TEXT NOT NULL,
    reason TEXT NOT NULL,
    timestamp INTEGER NOT NULL
);

-- Таблица: ipc_sequence_ledger (Anti-replay защита signer'а, хранится персистентно)
CREATE TABLE IF NOT EXISTS ipc_sequence_ledger (
    context_key TEXT PRIMARY KEY,
    last_ipc_seq INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Таблица: signer_audit_log (Аудит всех решений signer'а, атомарно с ipc_sequence_ledger)
CREATE TABLE IF NOT EXISTS signer_audit_log (
    id TEXT PRIMARY KEY,
    context_key TEXT NOT NULL,
    intent_type TEXT NOT NULL,
    ipc_seq INTEGER NOT NULL,
    verdict TEXT NOT NULL, -- accepted, rejected
    rejection_reason TEXT,
    tx_signature TEXT,
    timestamp INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_signer_audit_context ON signer_audit_log(context_key);

-- Таблица: wallet_watchlist (Smart Money кураторский список, Раздел 2.5)
CREATE TABLE IF NOT EXISTS wallet_watchlist (
    wallet_address TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    status TEXT NOT NULL, -- active, paused
    checklist_items_passed INTEGER NOT NULL,
    added_at TEXT NOT NULL,
    review_notes TEXT
);

-- Таблица: smart_money_hits (Зафиксированные покупки от смарт-мани)
CREATE TABLE IF NOT EXISTS smart_money_hits (
    id TEXT PRIMARY KEY,
    wallet_address TEXT NOT NULL REFERENCES wallet_watchlist(wallet_address),
    mint TEXT NOT NULL,
    tx_signature TEXT NOT NULL UNIQUE,
    detected_at_slot INTEGER NOT NULL
);

-- Таблица: scam_blacklist (Черный список адресов/токенов)
CREATE TABLE IF NOT EXISTS scam_blacklist (
    mint TEXT PRIMARY KEY,
    reason TEXT NOT NULL,
    added_at INTEGER NOT NULL
);
