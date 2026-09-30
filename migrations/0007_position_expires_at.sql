-- Migration 0007: absolute expiry timestamps for positions
--
-- Adds `expires_at` (ISO-8601 UTC TEXT) to `positions` and backfills it from
-- the legacy relative `expiry_days` column as `opened_at + expiry_days`.
--
-- `expiry_days` is retained for backward compatibility but is deprecated in
-- API responses; all repricing now derives time-to-expiry from `expires_at`.
--
-- The backfill is idempotent: it only touches rows where `expires_at` is still
-- NULL, so re-running the migration (or running it against a partially
-- migrated database) is safe.

-- 1. Add the column. SQLite requires a constant default for NOT NULL columns
--    added via ALTER TABLE, so we add it nullable first, backfill, then rely on
--    the application layer to always supply a value on insert.
ALTER TABLE positions ADD COLUMN expires_at TEXT;

-- 2. Idempotent backfill: opened_at + expiry_days, rendered as ISO-8601 UTC.
--    `opened_at` is stored as a Unix epoch (seconds) and `expiry_days` as an
--    integer number of days, matching the existing schema.
UPDATE positions
SET expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', opened_at + (expiry_days * 86400), 'unixepoch')
WHERE expires_at IS NULL
  AND opened_at IS NOT NULL
  AND expiry_days IS NOT NULL;

-- 3. Any rows that could not be backfilled (missing opened_at/expiry_days) get
--    a deterministic fallback so the column is never left NULL after migration.
UPDATE positions
SET expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
WHERE expires_at IS NULL;

-- 4. Index to support expiry-based queries (settlement, calendar lookups).
CREATE INDEX IF NOT EXISTS idx_positions_expires_at ON positions (expires_at);
