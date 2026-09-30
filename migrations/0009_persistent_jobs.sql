CREATE TABLE jobs (
    id             TEXT PRIMARY KEY,
    kind           TEXT NOT NULL,
    payload        TEXT NOT NULL DEFAULT '{}',
    status         TEXT NOT NULL DEFAULT 'queued'
                   CHECK (status IN ('queued', 'running', 'succeeded', 'dead')),
    scheduled_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    attempts       INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts   INTEGER NOT NULL DEFAULT 5 CHECK (max_attempts > 0),
    timeout_secs   INTEGER NOT NULL DEFAULT 60 CHECK (timeout_secs > 0),
    unique_key     TEXT,
    lease_owner    TEXT,
    lease_until    TEXT,
    last_error     TEXT,
    result_json    TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    started_at     TEXT,
    completed_at   TEXT
);

CREATE INDEX idx_jobs_ready ON jobs(status, scheduled_at);
CREATE INDEX idx_jobs_history ON jobs(created_at DESC, status);
CREATE UNIQUE INDEX idx_jobs_active_unique_key
    ON jobs(unique_key)
    WHERE unique_key IS NOT NULL AND status IN ('queued', 'running');

CREATE TABLE job_attempts (
    id             TEXT PRIMARY KEY,
    job_id         TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL,
    worker_id      TEXT NOT NULL,
    started_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    ended_at       TEXT,
    outcome        TEXT NOT NULL CHECK (outcome IN ('running', 'succeeded', 'retry', 'dead', 'timeout', 'lease_expired')),
    error          TEXT,
    UNIQUE (job_id, attempt_number)
);

CREATE INDEX idx_job_attempts_job ON job_attempts(job_id, attempt_number);

CREATE TABLE market_snapshots (
    id          TEXT PRIMARY KEY,
    captured_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    payload     TEXT NOT NULL
);

CREATE TABLE job_artifacts (
    job_id      TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    content_json TEXT NOT NULL
);

CREATE TABLE reconciliation_runs (
    id             TEXT PRIMARY KEY,
    job_id         TEXT NOT NULL REFERENCES jobs(id),
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    differences    INTEGER NOT NULL,
    result_json    TEXT NOT NULL
);
