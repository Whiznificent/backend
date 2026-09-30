-- Table to record the single active network this database is bound to.
-- Refuses startup if a process configured for a different network tries to connect.
CREATE TABLE IF NOT EXISTS network_metadata (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    network TEXT NOT NULL,
    passphrase TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
