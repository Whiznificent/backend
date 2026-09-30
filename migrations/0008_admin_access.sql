ALTER TABLE sessions ADD COLUMN admin_step_up_expires_at TEXT;

CREATE TABLE admin_roles (
    wallet_address TEXT NOT NULL,
    role           TEXT NOT NULL CHECK (role IN ('viewer', 'operator', 'risk_admin', 'super_admin')),
    granted_by     TEXT,
    granted_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (wallet_address, role)
);

CREATE TABLE admin_step_up_nonces (
    nonce          TEXT PRIMARY KEY,
    wallet_address TEXT NOT NULL,
    session_token  TEXT NOT NULL REFERENCES sessions(token) ON DELETE CASCADE,
    expires_at     TEXT NOT NULL
);

CREATE TABLE admin_series (
    id          TEXT PRIMARY KEY,
    underlying  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    active      INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (underlying, expires_at)
);

CREATE TABLE circuit_breakers (
    name        TEXT PRIMARY KEY,
    tripped     INTEGER NOT NULL CHECK (tripped IN (0, 1)),
    reason      TEXT NOT NULL DEFAULT '',
    changed_by  TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
