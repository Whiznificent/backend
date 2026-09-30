CREATE TABLE price_ticks (
    id          TEXT PRIMARY KEY,
    underlying  TEXT NOT NULL,
    price       REAL NOT NULL,
    observed_at TEXT NOT NULL
);

CREATE INDEX idx_price_ticks_symbol_time
    ON price_ticks(underlying, observed_at);
