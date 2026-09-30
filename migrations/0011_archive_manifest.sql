-- Archive manifest table for tracking purged and archived dataset batches
CREATE TABLE IF NOT EXISTS archive_manifests (
    id TEXT PRIMARY KEY,
    table_name TEXT NOT NULL,
    file_path TEXT NOT NULL,
    row_count INTEGER NOT NULL,
    checksum TEXT NOT NULL,
    from_timestamp TEXT NOT NULL,
    to_timestamp TEXT NOT NULL,
    archived_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
