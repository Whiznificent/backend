-- High-water marks for the analytics export pipeline (src/export.rs).
-- One row per exported dataset. The watermark is the last exported
-- updated_at (or minute) value; it is advanced only after a successful
-- upload, in the same transaction as the manifest write, so a run that
-- crashes before committing re-exports its rows instead of skipping them.
CREATE TABLE export_watermarks (
    dataset        TEXT PRIMARY KEY,
    watermark      TEXT NOT NULL,
    watermark_kind TEXT NOT NULL, -- 'updated_at' | 'minute' | 'none'
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
