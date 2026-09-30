-- Equity snapshots for account performance time-series (issue #36).
-- One row per active account per snapshot interval, holding the components
-- that make up account equity so the curve, drawdown, Sharpe-like ratio and
-- Greek attribution can be reconstructed from history.

CREATE TABLE IF NOT EXISTS equity_snapshots (
    wallet      TEXT        NOT NULL,
    ts          TIMESTAMPTZ NOT NULL,
    cash        NUMERIC(20, 8) NOT NULL DEFAULT 0,
    collateral  NUMERIC(20, 8) NOT NULL DEFAULT 0,
    unrealized  NUMERIC(20, 8) NOT NULL DEFAULT 0,
    equity      NUMERIC(20, 8) NOT NULL DEFAULT 0,
    PRIMARY KEY (wallet, ts)
);

-- Range queries always scan a single wallet ordered by time, so the primary
-- key already covers the hot path. This index keeps the batched snapshot job
-- (which looks up the latest snapshot per wallet) cheap as the table grows.
CREATE INDEX IF NOT EXISTS equity_snapshots_ts_idx
    ON equity_snapshots (ts DESC);
