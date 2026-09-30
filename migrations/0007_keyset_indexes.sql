-- Keyset (cursor) pagination support for list endpoints.
--
-- These composite indexes provide the stable ordering required by the
-- keyset cursors: rows are ordered by the sort key (DESC) with the primary
-- key `id` (DESC) as a deterministic tiebreaker. This lets the query planner
-- satisfy `WHERE wallet_address = ? AND (opened_at, id) < (?, ?)
-- ORDER BY opened_at DESC, id DESC` with an index range scan instead of an
-- O(offset) scan, and guarantees no duplicates or skips when rows are
-- inserted between page fetches.
--
-- Verified with `EXPLAIN QUERY PLAN` in tests (see src/pagination.rs).

-- /positions: ordered by (wallet_address, opened_at DESC, id DESC).
CREATE INDEX IF NOT EXISTS idx_positions_wallet_opened_at_id
    ON positions (wallet_address, opened_at DESC, id DESC);

-- /history: ordered by (wallet_address, created_at DESC, id DESC).
CREATE INDEX IF NOT EXISTS idx_history_wallet_created_at_id
    ON history (wallet_address, created_at DESC, id DESC);

-- /strategies: ordered by (wallet_address, created_at DESC, id DESC).
CREATE INDEX IF NOT EXISTS idx_strategies_wallet_created_at_id
    ON strategies (wallet_address, created_at DESC, id DESC);

-- /alerts: ordered by (wallet_address, created_at DESC, id DESC).
CREATE INDEX IF NOT EXISTS idx_alerts_wallet_created_at_id
    ON alerts (wallet_address, created_at DESC, id DESC);

-- Filter-composing indexes: the existing `status` and `strategy_id` filters
-- must compose with the cursor without breaking the index range scan.
CREATE INDEX IF NOT EXISTS idx_positions_wallet_status_opened_at_id
    ON positions (wallet_address, status, opened_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_positions_wallet_strategy_opened_at_id
    ON positions (wallet_address, strategy_id, opened_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_history_wallet_strategy_created_at_id
    ON history (wallet_address, strategy_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_alerts_wallet_status_created_at_id
    ON alerts (wallet_address, status, created_at DESC, id DESC);
