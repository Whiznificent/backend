-- CQRS read models for strategy and portfolio summaries. These tables are
-- maintained incrementally by the repository layer inside the same
-- transaction as the source mutation (see src/readmodels.rs), so
-- list_strategies and the history stats are served with O(page) lookups
-- instead of scanning and aggregating every leg on every request.
--
-- They are derived data: `zenith-admin readmodels rebuild` rebuilds them
-- from the positions table, and the consistency checker job reports any
-- drift between the two.

CREATE TABLE strategy_summaries (
    strategy_id     TEXT PRIMARY KEY,
    wallet_address  TEXT NOT NULL,
    underlying      TEXT NOT NULL,
    leg_count       INTEGER NOT NULL,
    open_leg_count  INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    opened_at       TEXT NOT NULL,
    realized_pnl    REAL NOT NULL DEFAULT 0.0
);

CREATE INDEX idx_strategy_summaries_wallet ON strategy_summaries(wallet_address);

CREATE TABLE wallet_position_counts (
    wallet_address      TEXT PRIMARY KEY,
    trade_count         INTEGER NOT NULL DEFAULT 0,
    win_count           INTEGER NOT NULL DEFAULT 0,
    loss_count          INTEGER NOT NULL DEFAULT 0,
    total_realized_pnl  REAL NOT NULL DEFAULT 0.0
);
