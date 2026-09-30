-- 0007_price_history.sql
-- Persist spot price time-series and rolled-up OHLCV candles.

CREATE TABLE IF NOT EXISTS price_ticks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    underlying  TEXT    NOT NULL,
    price       REAL    NOT NULL,
    source      TEXT    NOT NULL,
    observed_at INTEGER NOT NULL -- unix epoch seconds (UTC)
);

CREATE INDEX IF NOT EXISTS idx_price_ticks_underlying_observed
    ON price_ticks (underlying, observed_at);

CREATE TABLE IF NOT EXISTS price_candles (
    underlying   TEXT    NOT NULL,
    interval     TEXT    NOT NULL, -- '1m' | '5m' | '1h' | '1d'
    bucket_start INTEGER NOT NULL, -- unix epoch seconds, UTC-aligned
    open         REAL    NOT NULL,
    high         REAL    NOT NULL,
    low          REAL    NOT NULL,
    close        REAL    NOT NULL,
    tick_count   INTEGER NOT NULL DEFAULT 0,
    volume       REAL,             -- nullable: no trade volume yet
    PRIMARY KEY (underlying, interval, bucket_start)
);

CREATE INDEX IF NOT EXISTS idx_price_candles_lookup
    ON price_candles (underlying, interval, bucket_start);
