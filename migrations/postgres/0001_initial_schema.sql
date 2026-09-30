-- PostgreSQL equivalent schema for Zenith Options backend

CREATE TABLE IF NOT EXISTS accounts (
    wallet_address      TEXT PRIMARY KEY,
    balance             NUMERIC NOT NULL DEFAULT 100000.0,
    collateral_locked   NUMERIC NOT NULL DEFAULT 0.0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS positions (
    id                  TEXT PRIMARY KEY,
    wallet_address      TEXT NOT NULL REFERENCES accounts(wallet_address),
    underlying          TEXT NOT NULL,
    strike              NUMERIC NOT NULL,
    expiry_days         NUMERIC NOT NULL,
    option_type         TEXT NOT NULL CHECK (option_type IN ('call', 'put')),
    position_type       TEXT NOT NULL CHECK (position_type IN ('long', 'short')),
    contracts           NUMERIC NOT NULL,
    entry_premium       NUMERIC NOT NULL,
    entry_spot          NUMERIC NOT NULL,
    collateral          NUMERIC NOT NULL DEFAULT 0.0,
    status              TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed', 'rolled')),
    close_premium       NUMERIC,
    close_spot          NUMERIC,
    realized_pnl        NUMERIC,
    opened_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    closed_at           TIMESTAMPTZ,
    strategy_id         TEXT
);

CREATE TABLE IF NOT EXISTS watchlist (
    wallet_address      TEXT NOT NULL REFERENCES accounts(wallet_address),
    underlying          TEXT NOT NULL,
    added_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (wallet_address, underlying)
);

CREATE TABLE IF NOT EXISTS alerts (
    id                  TEXT PRIMARY KEY,
    wallet_address      TEXT NOT NULL REFERENCES accounts(wallet_address),
    underlying          TEXT NOT NULL,
    target_price        NUMERIC NOT NULL,
    direction           TEXT NOT NULL CHECK (direction IN ('above', 'below')),
    status              TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'triggered', 'dismissed')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    triggered_at        TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS auth_nonces (
    nonce               TEXT PRIMARY KEY,
    wallet_address      TEXT NOT NULL,
    expires_at          TIMESTAMPTZ NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS sessions (
    token               TEXT PRIMARY KEY,
    wallet_address      TEXT NOT NULL REFERENCES accounts(wallet_address),
    expires_at          TIMESTAMPTZ NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS network_metadata (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    network             TEXT NOT NULL,
    passphrase          TEXT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS sponsored_transactions (
    id                  TEXT PRIMARY KEY,
    inner_tx_hash       TEXT NOT NULL UNIQUE,
    wallet_address      TEXT NOT NULL,
    sponsor_account     TEXT NOT NULL,
    fee_charged         BIGINT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'submitted' CHECK (status IN ('submitted', 'confirmed', 'failed')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS contract_wasm_history (
    contract_id         TEXT NOT NULL,
    wasm_hash           TEXT NOT NULL,
    from_ledger         BIGINT NOT NULL,
    to_ledger           BIGINT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (contract_id, wasm_hash, from_ledger)
);

CREATE TABLE IF NOT EXISTS unprocessed_events (
    id                  TEXT PRIMARY KEY,
    contract_id         TEXT NOT NULL,
    wasm_hash           TEXT NOT NULL,
    ledger              BIGINT NOT NULL,
    topics              TEXT NOT NULL,
    data                TEXT NOT NULL,
    error_reason        TEXT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS leases (
    name                TEXT PRIMARY KEY,
    holder              TEXT NOT NULL,
    expires_at          BIGINT NOT NULL,
    acquired_at         BIGINT NOT NULL,
    renewed_at          BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS keeper_job_executions (
    job_id              TEXT PRIMARY KEY,
    task_name           TEXT NOT NULL,
    target_id           TEXT NOT NULL,
    status              TEXT NOT NULL CHECK (status IN ('pending', 'executing', 'completed', 'failed', 'skipped')),
    fee_spent           BIGINT NOT NULL DEFAULT 0,
    error_message       TEXT,
    due_at              BIGINT NOT NULL,
    executed_at         BIGINT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
