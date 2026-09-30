-- Migration 0009: Standardized Option Series Registry
-- Introduces a first-class option_series table and links positions to a
-- canonical instrument via a nullable series_id FK. Legacy rows keep NULL.

CREATE TABLE IF NOT EXISTS option_series (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    underlying      TEXT    NOT NULL,
    -- Strike stored as a scaled integer (strike_scaled = strike * 10^strike_scale)
    -- to avoid floating-point keys. strike_scale is the number of decimal places.
    strike_scaled   INTEGER NOT NULL,
    strike_scale    INTEGER NOT NULL DEFAULT 0,
    expiry          TEXT    NOT NULL,          -- ISO-8601 date (YYYY-MM-DD)
    expiry_kind     TEXT    NOT NULL,          -- daily | weekly | monthly | quarterly
    option_type     TEXT    NOT NULL,          -- call | put
    instrument_id   TEXT    NOT NULL,          -- e.g. BTC-27DEC26-70000-C
    status          TEXT    NOT NULL DEFAULT 'active', -- active | delisted | expired
    created_at      TEXT    NOT NULL DEFAULT (datetime('now')),
    delisted_at     TEXT,
    UNIQUE (underlying, expiry, strike_scaled, option_type)
);

CREATE INDEX IF NOT EXISTS idx_option_series_underlying
    ON option_series (underlying);
CREATE INDEX IF NOT EXISTS idx_option_series_expiry
    ON option_series (expiry);
CREATE INDEX IF NOT EXISTS idx_option_series_status
    ON option_series (status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_option_series_instrument_id
    ON option_series (instrument_id);

-- Link positions to a canonical series. Nullable so legacy rows stay NULL.
ALTER TABLE positions ADD COLUMN series_id INTEGER REFERENCES option_series(id);

CREATE INDEX IF NOT EXISTS idx_positions_series_id
    ON positions (series_id);
