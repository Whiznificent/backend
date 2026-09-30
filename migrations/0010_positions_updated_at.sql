-- updated_at gives the export pipeline a watermark for positions, which
-- are updated in place when they close or roll (status, close_premium,
-- close_spot and realized_pnl all fill in after the open). The write
-- paths in positions.rs maintain it on every insert and status
-- transition; existing rows are backfilled with their last-known change
-- time so the first export after this migration has a sane watermark.
ALTER TABLE positions ADD COLUMN updated_at TEXT;

UPDATE positions
   SET updated_at = COALESCE(closed_at, opened_at)
 WHERE updated_at IS NULL;
