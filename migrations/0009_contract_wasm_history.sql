-- Track WASM hash version history per contract across ledger ranges
CREATE TABLE IF NOT EXISTS contract_wasm_history (
    contract_id TEXT NOT NULL,
    wasm_hash TEXT NOT NULL,
    from_ledger INTEGER NOT NULL,
    to_ledger INTEGER,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (contract_id, wasm_hash, from_ledger)
);

-- Store raw unparseable / failed events for diagnostic inspection without corrupting projections
CREATE TABLE IF NOT EXISTS unprocessed_events (
    id TEXT PRIMARY KEY,
    contract_id TEXT NOT NULL,
    wasm_hash TEXT NOT NULL,
    ledger INTEGER NOT NULL,
    topics TEXT NOT NULL,
    data TEXT NOT NULL,
    error_reason TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
