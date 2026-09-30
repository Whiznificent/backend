-- Migration 0008: Automated Expiry Settlement Engine
--
-- Adds:
--   * a new terminal position status 'expired' (allowed by the positions CHECK constraint)
--   * a settlement_fixings table that records the deterministic settlement price
--     (TWAP over the 30 minutes before expiry, or a fallback to the last
--     aggregated price) for each (underlying, expires_at) pair.
--
-- The fixing is persisted before any position is settled so that settlement is
-- idempotent and resumable: a crash between the fixing phase and the settlement
-- phase can be retried without recomputing (or double-crediting) anything.

-- 1. Allow the new terminal status on positions.
--    The original constraint only permitted 'open' and 'closed'.
ALTER TABLE positions
    DROP CONSTRAINT IF EXISTS positions_status_check;

ALTER TABLE positions
    ADD CONSTRAINT positions_status_check
    CHECK (status IN ('open', 'closed', 'expired'));

-- 2. Persisted settlement fixings, one row per (underlying, expires_at).
CREATE TABLE IF NOT EXISTS settlement_fixings (
    id          BIGSERIAL PRIMARY KEY,
    underlying  TEXT        NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    price       NUMERIC     NOT NULL,
    method      TEXT        NOT NULL
        CHECK (method IN ('twap', 'fallback')),
    tick_count  INTEGER     NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT settlement_fixings_underlying_expires_at_key
        UNIQUE (underlying, expires_at)
);

-- Lookups by expiry window (e.g. GET /api/v1/settlements?underlying=&expires_at=).
CREATE INDEX IF NOT EXISTS settlement_fixings_expires_at_idx
    ON settlement_fixings (expires_at);
