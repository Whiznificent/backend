-- Migration: 0007_competitions.sql
-- Leaderboard and Trading Competition Service with Sybil Resistance (issue #38)
--
-- Adds the persistence layer for paper-trading competitions:
--   * competitions        - admin-defined windows, eligible underlyings, scoring metric
--   * competition_entries - wallet opt-in, session/IP capture for sybil heuristics
--   * competition_scores  - snapshotted, incrementally recomputed standings
--
-- The leaderboard endpoint serves ranked results from competition_scores so that
-- reads never have to recompute positions on the hot path.

BEGIN;

-- ---------------------------------------------------------------------------
-- competitions
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS competitions (
    id                  BIGSERIAL PRIMARY KEY,
    slug                TEXT        NOT NULL UNIQUE,
    name                TEXT        NOT NULL,
    description         TEXT,

    -- Time window. Scoring only counts positions opened inside [starts_at, ends_at).
    starts_at           TIMESTAMPTZ NOT NULL,
    ends_at             TIMESTAMPTZ NOT NULL,

    -- Eligible underlyings (e.g. {BTC,ETH,SOL}); empty array means "all".
    eligible_underlyings TEXT[]     NOT NULL DEFAULT '{}',

    -- Scoring metric: 'roi' | 'pnl' | 'risk_adjusted'.
    scoring_metric      TEXT        NOT NULL DEFAULT 'roi',

    -- Sybil heuristic tuning: wallets sharing more than this many distinct
    -- session IPs (or sharing an IP with this many other entries) are flagged.
    sybil_ip_threshold  INTEGER     NOT NULL DEFAULT 3,

    -- Number of minutes between incremental snapshot recomputations.
    recompute_interval_minutes INTEGER NOT NULL DEFAULT 15,

    -- 'draft' | 'active' | 'ended' | 'archived'
    status              TEXT        NOT NULL DEFAULT 'draft',

    created_by          BIGINT      REFERENCES users(id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT competitions_window_valid CHECK (ends_at > starts_at),
    CONSTRAINT competitions_metric_valid CHECK (
        scoring_metric IN ('roi', 'pnl', 'risk_adjusted')
    ),
    CONSTRAINT competitions_status_valid CHECK (
        status IN ('draft', 'active', 'ended', 'archived')
    )
);

CREATE INDEX IF NOT EXISTS idx_competitions_status_window
    ON competitions (status, starts_at, ends_at);

-- ---------------------------------------------------------------------------
-- competition_entries
-- ---------------------------------------------------------------------------
-- One row per wallet that opts in. Session IPs are captured at verify time
-- (see src/auth.rs) and stored here so the sybil heuristics can cluster wallets.
CREATE TABLE IF NOT EXISTS competition_entries (
    id                  BIGSERIAL PRIMARY KEY,
    competition_id      BIGINT      NOT NULL REFERENCES competitions(id) ON DELETE CASCADE,
    user_id             BIGINT      NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- Wallet address used for the competition (truncated on the public board).
    wallet_address      TEXT        NOT NULL,

    -- Distinct session IPs observed for this entry during the window.
    session_ips         TEXT[]      NOT NULL DEFAULT '{}',

    -- Epoch guards against account resets mid-competition: a reset bumps the
    -- epoch and the entry is disqualified rather than silently rescored.
    epoch               INTEGER     NOT NULL DEFAULT 0,

    -- Sybil review state: 'clean' | 'flagged' | 'approved' | 'disqualified'.
    -- Flagged entries are hidden from the public board until an admin reviews.
    review_status       TEXT        NOT NULL DEFAULT 'clean',
    review_note         TEXT,
    reviewed_by         BIGINT      REFERENCES users(id) ON DELETE SET NULL,
    reviewed_at         TIMESTAMPTZ,

    -- Deterministic tie-break: earlier entry wins.
    entered_at          TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT competition_entries_review_valid CHECK (
        review_status IN ('clean', 'flagged', 'approved', 'disqualified')
    ),
    CONSTRAINT competition_entries_unique_wallet
        UNIQUE (competition_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_competition_entries_competition
    ON competition_entries (competition_id, entered_at);
CREATE INDEX IF NOT EXISTS idx_competition_entries_review
    ON competition_entries (competition_id, review_status);
-- GIN index supports "wallets sharing session IPs" clustering queries.
CREATE INDEX IF NOT EXISTS idx_competition_entries_session_ips
    ON competition_entries USING GIN (session_ips);

-- ---------------------------------------------------------------------------
-- competition_scores (snapshotted)
-- ---------------------------------------------------------------------------
-- Recomputed every N minutes; the leaderboard endpoint reads the latest
-- snapshot per entry. Keeping history lets admins audit how standings moved.
CREATE TABLE IF NOT EXISTS competition_scores (
    id                  BIGSERIAL PRIMARY KEY,
    competition_id      BIGINT      NOT NULL REFERENCES competitions(id) ON DELETE CASCADE,
    entry_id            BIGINT      NOT NULL REFERENCES competition_entries(id) ON DELETE CASCADE,

    -- Snapshot generation timestamp; the newest per entry is the live score.
    snapshot_at         TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Raw metric value for the competition's scoring_metric.
    score               NUMERIC(28, 8) NOT NULL DEFAULT 0,

    -- Supporting figures for auditing / display.
    realized_pnl        NUMERIC(28, 8) NOT NULL DEFAULT 0,
    unrealized_pnl      NUMERIC(28, 8) NOT NULL DEFAULT 0,
    roi                 NUMERIC(28, 8) NOT NULL DEFAULT 0,
    risk_adjusted       NUMERIC(28, 8) NOT NULL DEFAULT 0,
    positions_counted   INTEGER     NOT NULL DEFAULT 0,

    -- Rank within the snapshot (1-based); NULL when the entry is hidden.
    rank                INTEGER,

    CONSTRAINT competition_scores_unique_snapshot
        UNIQUE (competition_id, entry_id, snapshot_at)
);

CREATE INDEX IF NOT EXISTS idx_competition_scores_latest
    ON competition_scores (competition_id, entry_id, snapshot_at DESC);
CREATE INDEX IF NOT EXISTS idx_competition_scores_rank
    ON competition_scores (competition_id, snapshot_at DESC, rank);

COMMIT;
