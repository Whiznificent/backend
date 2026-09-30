-- Reconciliation job persistence: runs and per-item discrepancies.
-- Compares indexed on-chain state against backend positions/accounts projections.

CREATE TABLE IF NOT EXISTS reconciliation_runs (
    id              BIGSERIAL PRIMARY KEY,
    ledger_sequence BIGINT NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at     TIMESTAMPTZ,
    status          TEXT NOT NULL DEFAULT 'running',
    total_items     BIGINT NOT NULL DEFAULT 0,
    critical_items  BIGINT NOT NULL DEFAULT 0,
    warning_items   BIGINT NOT NULL DEFAULT 0,
    info_items      BIGINT NOT NULL DEFAULT 0,
    auto_heal       BOOLEAN NOT NULL DEFAULT FALSE,
    healed_items    BIGINT NOT NULL DEFAULT 0,
    error           TEXT,
    CONSTRAINT reconciliation_runs_status_check
        CHECK (status IN ('running', 'completed', 'failed'))
);

CREATE INDEX IF NOT EXISTS reconciliation_runs_ledger_idx
    ON reconciliation_runs (ledger_sequence DESC);
CREATE INDEX IF NOT EXISTS reconciliation_runs_started_idx
    ON reconciliation_runs (started_at DESC);

CREATE TABLE IF NOT EXISTS reconciliation_items (
    id              BIGSERIAL PRIMARY KEY,
    run_id          BIGINT NOT NULL REFERENCES reconciliation_runs (id) ON DELETE CASCADE,
    category        TEXT NOT NULL,
    severity        TEXT NOT NULL,
    wallet          TEXT,
    series_id       TEXT,
    asset           TEXT,
    onchain_amount  NUMERIC(38, 0),
    offchain_amount NUMERIC(38, 0),
    onchain_status  TEXT,
    offchain_status TEXT,
    details         JSONB NOT NULL DEFAULT '{}'::jsonb,
    healed          BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT reconciliation_items_category_check
        CHECK (category IN ('missing_offchain', 'missing_onchain', 'amount_mismatch', 'status_mismatch')),
    CONSTRAINT reconciliation_items_severity_check
        CHECK (severity IN ('critical', 'warning', 'info'))
);

CREATE INDEX IF NOT EXISTS reconciliation_items_run_idx
    ON reconciliation_items (run_id);
CREATE INDEX IF NOT EXISTS reconciliation_items_category_idx
    ON reconciliation_items (category, severity);
CREATE INDEX IF NOT EXISTS reconciliation_items_wallet_idx
    ON reconciliation_items (wallet, series_id);
