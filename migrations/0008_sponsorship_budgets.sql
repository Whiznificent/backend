-- Ledger for fee-bump sponsored transactions and budget tracking
CREATE TABLE IF NOT EXISTS sponsored_transactions (
    id TEXT PRIMARY KEY,
    inner_tx_hash TEXT NOT NULL UNIQUE,
    wallet_address TEXT NOT NULL,
    sponsor_account TEXT NOT NULL,
    fee_charged INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'submitted' CHECK (status IN ('submitted', 'confirmed', 'failed')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_sponsored_wallet ON sponsored_transactions(wallet_address, created_at);
CREATE INDEX IF NOT EXISTS idx_sponsored_created ON sponsored_transactions(created_at);
