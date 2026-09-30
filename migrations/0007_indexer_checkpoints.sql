-- 0007_indexer_checkpoints.sql
-- Indexer checkpointing, gap detection, historical replay and projection
-- schema versioning. Raw events are immutable; projections are derived data
-- and may be truncated and rebuilt idempotently.

-- ---------------------------------------------------------------------------
-- Processed ledger ranges (checkpoints)
-- ---------------------------------------------------------------------------
-- Every processed ledger range is recorded here. A checker can then find
-- non-contiguous ranges (gaps) and raise an alert. Ranges are half-open:
-- [from_ledger, to_ledger).
CREATE TABLE IF NOT EXISTS indexer_checkpoints (
    id              BIGSERIAL PRIMARY KEY,
    from_ledger     BIGINT      NOT NULL,
    to_ledger       BIGINT      NOT NULL,
    source          TEXT        NOT NULL DEFAULT 'rpc',
    status          TEXT        NOT NULL DEFAULT 'complete',
    event_count     BIGINT      NOT NULL DEFAULT 0,
    checksum        TEXT,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ,
    CONSTRAINT indexer_checkpoints_range_valid CHECK (to_ledger > from_ledger),
    CONSTRAINT indexer_checkpoints_status_valid
        CHECK (status IN ('in_progress', 'complete', 'failed'))
);

-- Fast lookup of the latest contiguous checkpoint and gap scans.
CREATE INDEX IF NOT EXISTS indexer_checkpoints_range_idx
    ON indexer_checkpoints (from_ledger, to_ledger);
CREATE INDEX IF NOT EXISTS indexer_checkpoints_status_idx
    ON indexer_checkpoints (status);

-- ---------------------------------------------------------------------------
-- Gap alerts
-- ---------------------------------------------------------------------------
-- Raised by the contiguity checker when a hole is detected between two
-- recorded ranges. Operators resolve them after a replay/backfill.
CREATE TABLE IF NOT EXISTS indexer_gap_alerts (
    id              BIGSERIAL PRIMARY KEY,
    gap_from_ledger BIGINT      NOT NULL,
    gap_to_ledger   BIGINT      NOT NULL,
    detected_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at     TIMESTAMPTZ,
    details         TEXT,
    CONSTRAINT indexer_gap_alerts_range_valid CHECK (gap_to_ledger > gap_from_ledger)
);

CREATE INDEX IF NOT EXISTS indexer_gap_alerts_unresolved_idx
    ON indexer_gap_alerts (gap_from_ledger)
    WHERE resolved_at IS NULL;

-- ---------------------------------------------------------------------------
-- Replay runs
-- ---------------------------------------------------------------------------
-- Tracks `zenith-admin indexer replay` invocations. The advisory lock row
-- (see indexer_replay_lock) serialises replay against live ingestion.
CREATE TABLE IF NOT EXISTS indexer_replay_runs (
    id              BIGSERIAL PRIMARY KEY,
    from_ledger     BIGINT      NOT NULL,
    to_ledger       BIGINT      NOT NULL,
    projections_only BOOLEAN    NOT NULL DEFAULT false,
    status          TEXT        NOT NULL DEFAULT 'running',
    events_replayed BIGINT      NOT NULL DEFAULT 0,
    checksum        TEXT,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ,
    error           TEXT,
    CONSTRAINT indexer_replay_runs_range_valid CHECK (to_ledger >= from_ledger),
    CONSTRAINT indexer_replay_runs_status_valid
        CHECK (status IN ('running', 'complete', 'failed'))
);

-- Single-row lock table used to pause live projection while a replay runs.
CREATE TABLE IF NOT EXISTS indexer_replay_lock (
    id          SMALLINT    PRIMARY KEY DEFAULT 1,
    locked_by   TEXT,
    locked_at   TIMESTAMPTZ,
    CONSTRAINT indexer_replay_lock_singleton CHECK (id = 1)
);

INSERT INTO indexer_replay_lock (id) VALUES (1)
    ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- Projection schema versioning (shadow tables + swap)
-- ---------------------------------------------------------------------------
-- A version bump triggers a background rebuild into shadow tables; once the
-- rebuild completes the shadow tables are swapped in without downtime.
CREATE TABLE IF NOT EXISTS projection_schema_versions (
    projection      TEXT        PRIMARY KEY,
    active_version  INTEGER     NOT NULL DEFAULT 1,
    target_version  INTEGER,
    status          TEXT        NOT NULL DEFAULT 'active',
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT projection_schema_versions_status_valid
        CHECK (status IN ('active', 'rebuilding', 'swapping', 'failed'))
);

-- ---------------------------------------------------------------------------
-- LedgerMeta backfill source (Galexie-style bucket)
-- ---------------------------------------------------------------------------
-- Records the LedgerMeta objects fetched from a Galexie-style bucket when the
-- RPC retention window has passed. The backfill source itself sits behind a
-- trait in the indexer crate; this table only persists what was fetched.
CREATE TABLE IF NOT EXISTS ledger_meta_backfill (
    ledger_sequence BIGINT      PRIMARY KEY,
    bucket_path     TEXT        NOT NULL,
    checksum        TEXT,
    fetched_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ledger_meta_backfill_fetched_idx
    ON ledger_meta_backfill (fetched_at);
