-- 0002_align_with_tz_v1.sql
-- Aligning schema with TZ section 5.1

-- 1. positions table
ALTER TABLE positions RENAME COLUMN id TO position_id;
ALTER TABLE positions ADD COLUMN dex_program_id TEXT NOT NULL DEFAULT '';
ALTER TABLE positions ADD COLUMN size_pct_of_capital REAL NOT NULL DEFAULT 0.0;
ALTER TABLE positions ADD COLUMN remaining_token_pct REAL NOT NULL DEFAULT 1.0;
ALTER TABLE positions ADD COLUMN hard_sl_price_sol REAL NOT NULL DEFAULT 0.0;
ALTER TABLE positions ADD COLUMN trailing_active INTEGER NOT NULL DEFAULT 0;
ALTER TABLE positions ADD COLUMN trailing_high_water_sol REAL;
ALTER TABLE positions RENAME COLUMN realized_pnl_sol TO realized_pnl_net_sol;
ALTER TABLE positions RENAME COLUMN created_at TO opened_at_utc;
ALTER TABLE positions RENAME COLUMN updated_at TO closed_at_utc;

-- 2. state_transitions table
ALTER TABLE state_transitions ADD COLUMN tx_signature TEXT;
ALTER TABLE state_transitions RENAME COLUMN timestamp TO created_at_utc;

-- 3. signer_audit_log table
ALTER TABLE signer_audit_log ADD COLUMN position_id TEXT;
ALTER TABLE signer_audit_log ADD COLUMN issued_at_unix_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE signer_audit_log RENAME COLUMN timestamp TO received_at_utc;
ALTER TABLE signer_audit_log RENAME COLUMN tx_signature TO constructed_tx_signature;

-- 4. smart_money_hits table
ALTER TABLE smart_money_hits ADD COLUMN pool_address TEXT NOT NULL DEFAULT '';
ALTER TABLE smart_money_hits ADD COLUMN created_at_utc INTEGER NOT NULL DEFAULT 0;

-- 5. scam_blacklist table
ALTER TABLE scam_blacklist ADD COLUMN source TEXT;
ALTER TABLE scam_blacklist RENAME COLUMN added_at TO added_at_utc;

-- 6. wallet_watchlist table
ALTER TABLE wallet_watchlist RENAME COLUMN added_at TO added_at_utc;
ALTER TABLE wallet_watchlist ADD COLUMN last_reviewed_at_utc INTEGER NOT NULL DEFAULT 0;
