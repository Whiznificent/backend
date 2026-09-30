CREATE TABLE api_keys (
    id              TEXT PRIMARY KEY,
    wallet_address  TEXT NOT NULL REFERENCES accounts(wallet_address),
    secret          TEXT NOT NULL,
    scopes          TEXT NOT NULL,
    ip_allowlist    TEXT,
    expires_at      TEXT,
    label           TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_api_keys_wallet ON api_keys(wallet_address);
