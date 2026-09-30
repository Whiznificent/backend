-- Distributed lease table for keeper leader election using DB-evaluated timestamps
CREATE TABLE IF NOT EXISTS leases (
    name        TEXT PRIMARY KEY,
    holder      TEXT NOT NULL,
    expires_at  INTEGER NOT NULL,
    acquired_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    renewed_at  INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

-- Keeper job execution records for auditability, metrics, and idempotency
CREATE TABLE IF NOT EXISTS keeper_job_executions (
    job_id          TEXT PRIMARY KEY,
    task_name       TEXT NOT NULL,
    target_id       TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('pending', 'executing', 'completed', 'failed', 'skipped')),
    fee_spent       INTEGER NOT NULL DEFAULT 0,
    error_message   TEXT,
    due_at          INTEGER NOT NULL,
    executed_at     INTEGER,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_keeper_task_target ON keeper_job_executions(task_name, target_id);
