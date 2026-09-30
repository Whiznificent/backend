-- Migration: protocol stats snapshots
-- Stores hourly snapshots of derived protocol analytics so that
-- GET /api/v1/stats can serve the latest snapshot without scanning the
-- full positions table on the request path, and so that trend charts can
-- be served from GET /api/v1/stats/history?metric=&from=&to=.

-- Indexed windowed aggregation over the positions ledger.
-- Supports premium/notional volume, unique traders, positions opened and
-- active series queries filtered by opened_at over 24h / 7d / all-time.
CREATE INDEX IF NOT EXISTS idx_positions_opened_at
    ON positions (opened_at);

-- Composite index to support per-underlying open interest recompute and
-- windowed aggregation grouped by underlying.
CREATE INDEX IF NOT EXISTS idx_positions_underlying_opened_at
    ON positions (underlying, opened_at);

-- Hourly snapshots of derived protocol metrics.
-- One row per (snapshot_at, metric) so that new metrics can be added
-- without schema changes and history queries can filter by metric name.
CREATE TABLE IF NOT EXISTS protocol_stats_snapshots (
    id           BIGSERIAL PRIMARY KEY,
    snapshot_at  TIMESTAMPTZ NOT NULL,
    metric       TEXT        NOT NULL,
    value        DOUBLE PRECISION NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fast lookup of the latest snapshot per metric and range scans for history.
CREATE UNIQUE INDEX IF NOT EXISTS idx_protocol_stats_snapshots_metric_at
    ON protocol_stats_snapshots (metric, snapshot_at);

CREATE INDEX IF NOT EXISTS idx_protocol_stats_snapshots_at
    ON protocol_stats_snapshots (snapshot_at DESC);

-- Per-underlying open interest snapshots (open contracts x spot), stored
-- alongside the aggregate metrics so live OI recompute can be compared and
-- delisted underlyings can be excluded from live OI while remaining in
-- historical volume.
CREATE TABLE IF NOT EXISTS protocol_stats_underlying_snapshots (
    id           BIGSERIAL PRIMARY KEY,
    snapshot_at  TIMESTAMPTZ NOT NULL,
    underlying   TEXT        NOT NULL,
    open_interest DOUBLE PRECISION NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_protocol_stats_underlying_snapshots_key
    ON protocol_stats_underlying_snapshots (underlying, snapshot_at);

CREATE INDEX IF NOT EXISTS idx_protocol_stats_underlying_snapshots_at
    ON protocol_stats_underlying_snapshots (snapshot_at DESC);
