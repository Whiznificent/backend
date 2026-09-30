CREATE TABLE feature_flags (
    environment     TEXT NOT NULL,
    name            TEXT NOT NULL,
    enabled         INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    rollout_percent REAL NOT NULL DEFAULT 0 CHECK (rollout_percent BETWEEN 0 AND 100),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (environment, name)
);

CREATE TABLE feature_flag_wallets (
    environment     TEXT NOT NULL,
    flag_name       TEXT NOT NULL,
    wallet_address  TEXT NOT NULL,
    PRIMARY KEY (environment, flag_name, wallet_address),
    FOREIGN KEY (environment, flag_name)
        REFERENCES feature_flags(environment, name) ON DELETE CASCADE
);

CREATE INDEX idx_feature_flag_wallets_flag
    ON feature_flag_wallets(environment, flag_name);
