DROP INDEX idx_alerts_wallet;
DROP INDEX idx_alerts_untriggered;
ALTER TABLE alerts RENAME TO alerts_legacy;

CREATE TABLE alerts (
    id              TEXT PRIMARY KEY,
    wallet_address  TEXT NOT NULL REFERENCES accounts(wallet_address),
    underlying      TEXT NOT NULL,
    condition       TEXT NOT NULL CHECK (condition IN (
        'above', 'below',
        'percent_change_above', 'percent_change_below',
        'iv_above', 'iv_below',
        'position_pnl_above', 'position_pnl_below',
        'strategy_pnl_above', 'strategy_pnl_below',
        'portfolio_delta_above', 'portfolio_delta_below', 'portfolio_delta_outside',
        'expiry_within'
    )),
    target_price    REAL NOT NULL,
    triggered       INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    triggered_at    TEXT,
    window_seconds  INTEGER,
    strike          REAL,
    expiry_days     REAL,
    option_type     TEXT CHECK (option_type IS NULL OR option_type IN ('call', 'put')),
    position_id     TEXT,
    strategy_id     TEXT,
    trigger_policy  TEXT NOT NULL DEFAULT 'once' CHECK (trigger_policy IN ('once', 'recurring', 'auto_rearm')),
    cooldown_seconds INTEGER NOT NULL DEFAULT 0 CHECK (cooldown_seconds >= 0),
    last_triggered_at TEXT,
    lower_bound REAL,
    upper_bound REAL
);

INSERT INTO alerts (id, wallet_address, underlying, condition, target_price,
                    triggered, created_at, triggered_at, trigger_policy)
SELECT id, wallet_address, underlying, condition, target_price,
       triggered, created_at, triggered_at, 'once'
FROM alerts_legacy;
DROP TABLE alerts_legacy;

CREATE INDEX idx_alerts_wallet ON alerts(wallet_address);
CREATE INDEX idx_alerts_untriggered ON alerts(underlying, triggered) WHERE triggered = 0;
CREATE INDEX idx_alerts_due ON alerts(triggered, trigger_policy, last_triggered_at);

CREATE TABLE spot_history (
    underlying TEXT NOT NULL,
    price REAL NOT NULL,
    recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (underlying, recorded_at)
);
CREATE INDEX idx_spot_history_lookup ON spot_history(underlying, recorded_at);
