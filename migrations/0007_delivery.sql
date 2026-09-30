CREATE TABLE api_keys (
    id              TEXT PRIMARY KEY,
    wallet_address  TEXT NOT NULL REFERENCES accounts(wallet_address) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    key_hash        TEXT NOT NULL UNIQUE,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_api_keys_wallet ON api_keys(wallet_address);

CREATE TABLE delivery_channels (
    id                  TEXT PRIMARY KEY,
    wallet_address      TEXT NOT NULL REFERENCES accounts(wallet_address) ON DELETE CASCADE,
    channel             TEXT NOT NULL CHECK (channel IN ('email', 'telegram', 'discord')),
    destination         TEXT NOT NULL,
    verified            INTEGER NOT NULL DEFAULT 0 CHECK (verified IN (0, 1)),
    verification_hash   TEXT,
    verification_expires_at TEXT,
    verification_attempts INTEGER NOT NULL DEFAULT 0 CHECK (verification_attempts BETWEEN 0 AND 5),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(wallet_address, channel, destination)
);

CREATE INDEX idx_delivery_channels_wallet ON delivery_channels(wallet_address);

CREATE TABLE webhook_endpoints (
    id              TEXT PRIMARY KEY,
    wallet_address  TEXT NOT NULL REFERENCES accounts(wallet_address) ON DELETE CASCADE,
    url             TEXT NOT NULL,
    secret          TEXT NOT NULL,
    event_types     TEXT NOT NULL,
    active          INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_webhook_endpoints_wallet ON webhook_endpoints(wallet_address);

CREATE TABLE delivery_events (
    id              TEXT PRIMARY KEY,
    wallet_address  TEXT NOT NULL REFERENCES accounts(wallet_address) ON DELETE CASCADE,
    event_type      TEXT NOT NULL,
    payload         TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE delivery_attempts (
    id                  TEXT PRIMARY KEY,
    event_id            TEXT NOT NULL REFERENCES delivery_events(id) ON DELETE CASCADE,
    endpoint_id         TEXT,
    channel_id          TEXT,
    attempts            INTEGER NOT NULL DEFAULT 0,
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'delivered', 'failed')),
    next_attempt_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_error          TEXT,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    delivered_at        TEXT,
    CHECK ((endpoint_id IS NOT NULL AND channel_id IS NULL) OR
           (endpoint_id IS NULL AND channel_id IS NOT NULL))
);

CREATE UNIQUE INDEX idx_delivery_attempt_endpoint_event
    ON delivery_attempts(event_id, endpoint_id) WHERE endpoint_id IS NOT NULL;
CREATE UNIQUE INDEX idx_delivery_attempt_channel_event
    ON delivery_attempts(event_id, channel_id) WHERE channel_id IS NOT NULL;
CREATE INDEX idx_delivery_attempts_due ON delivery_attempts(status, next_attempt_at);
