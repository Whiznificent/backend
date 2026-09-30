-- OHLC spot-price candles, persisted once per minute per underlying by
-- the price simulator (src/prices.rs). This is the source for the
-- price_candles analytics export dataset — without it there is no
-- historical price data to export (spot prices otherwise live only in
-- memory).
CREATE TABLE price_candles (
    underlying  TEXT NOT NULL,
    minute      TEXT NOT NULL, -- ISO timestamp of the minute bucket
    open        REAL NOT NULL,
    high        REAL NOT NULL,
    low         REAL NOT NULL,
    close       REAL NOT NULL,
    PRIMARY KEY (underlying, minute)
);
