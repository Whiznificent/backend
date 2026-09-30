-- Transaction submission and lifecycle tracking.
-- Persists the state machine for every submitted Soroban transaction so that
-- clients can poll GET /api/v1/tx/:hash and the background poller can resume
-- after a restart.

CREATE TABLE IF NOT EXISTS chain_txs (
    hash            TEXT        PRIMARY KEY,
    wallet          TEXT        NOT NULL,
    kind            TEXT        NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'PENDING',
    submitted_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    ledger          BIGINT,
    result_xdr      TEXT,
    error           TEXT,
    -- Ledger after which the transaction can no longer be included; used by the
    -- poller to mark stale transactions as expired.
    valid_until_ledger BIGINT,
    -- Bookkeeping for the background poller (backoff + restart recovery).
    last_polled_at  TIMESTAMPTZ,
    poll_attempts   INTEGER     NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT chain_txs_status_check
        CHECK (status IN ('PENDING', 'SUCCESS', 'FAILED', 'NOT_FOUND', 'EXPIRED'))
);

-- Poller scans pending transactions ordered by submission time.
CREATE INDEX IF NOT EXISTS chain_txs_status_submitted_at_idx
    ON chain_txs (status, submitted_at);

-- Clients look up transactions by wallet.
CREATE INDEX IF NOT EXISTS chain_txs_wallet_idx
    ON chain_txs (wallet);
