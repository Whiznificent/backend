-- 0007_quotes.sql
-- Time-bound executable quotes for RFQ quote locking (issue #34).
--
-- Quotes are stored server-side so that single-use (replay) protection can be
-- enforced atomically inside the execution transaction via
--   UPDATE quotes SET used_at = ? WHERE id = ? AND used_at IS NULL
-- A stateless HMAC design cannot guarantee exactly-once consumption without
-- shared state, so we persist the quote and mark it consumed on execution.

CREATE TABLE IF NOT EXISTS quotes (
    id              TEXT PRIMARY KEY,
    wallet          TEXT NOT NULL,
    legs            TEXT NOT NULL,          -- canonical JSON of legs (series, side, size)
    size            TEXT NOT NULL,          -- decimal as string to avoid float drift
    side            TEXT NOT NULL,          -- 'buy' | 'sell'
    premium         TEXT NOT NULL,          -- quoted premium, decimal as string
    spot_at_issue   TEXT NOT NULL,          -- spot when the quote was issued
    issued_at       INTEGER NOT NULL,       -- unix seconds (server clock only)
    expires_at      INTEGER NOT NULL,       -- unix seconds (server clock only)
    used_at         INTEGER,                -- NULL until consumed (replay guard)
    signature       TEXT NOT NULL           -- HMAC over the immutable fields
);

-- Fast lookup by id for the execution path.
CREATE INDEX IF NOT EXISTS idx_quotes_id ON quotes (id);

-- Support wallet-scoped auditing / cleanup of stale quotes.
CREATE INDEX IF NOT EXISTS idx_quotes_wallet ON quotes (wallet);

-- Support periodic pruning of expired quotes.
CREATE INDEX IF NOT EXISTS idx_quotes_expires_at ON quotes (expires_at);
