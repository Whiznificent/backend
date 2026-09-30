-- Idempotency keys for mutating endpoints (issue #28)
-- Stores the first response for a (wallet, key) pair so retried requests
-- with the same key and body hash replay the stored response instead of
-- executing the mutation a second time.

CREATE TABLE IF NOT EXISTS idempotency_keys (
    id            BIGSERIAL PRIMARY KEY,
    wallet        TEXT        NOT NULL,
    key           TEXT        NOT NULL,
    request_hash  TEXT        NOT NULL,
    status_code   INTEGER,
    response_body TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT idempotency_keys_wallet_key_unique UNIQUE (wallet, key)
);

-- Fast lookup for the cleanup sweeper that expires keys after 24h.
CREATE INDEX IF NOT EXISTS idempotency_keys_created_at_idx
    ON idempotency_keys (created_at);
