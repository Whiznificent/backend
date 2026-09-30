-- 0008: price-tick history.
--
-- One row per price-simulator tick: the spot/vol snapshot broadcast to
-- WebSocket clients every ~2s. This is the high-volume table the
-- performance regression suite seeds with ~1M rows to keep full scans
-- measurably slow, and it backs any future tick-history/chart feature.
CREATE TABLE ticks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    underlying  TEXT NOT NULL,
    spot        REAL NOT NULL,
    vol         REAL NOT NULL,
    tick_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_ticks_underlying ON ticks(underlying);
CREATE INDEX idx_ticks_underlying_time ON ticks(underlying, tick_at);
