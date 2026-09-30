-- Migration 0007: collateral_adjustments audit table
-- Records every dynamic collateral re-margin adjustment for open short positions.
-- See issue #41: Dynamic Collateral Re-Margining on Spot Moves.

CREATE TABLE IF NOT EXISTS collateral_adjustments (
    id              BIGSERIAL PRIMARY KEY,
    wallet_id       BIGINT      NOT NULL,
    position_id     BIGINT      NOT NULL,
    direction       TEXT        NOT NULL CHECK (direction IN ('top_up', 'release')),
    locked_before   NUMERIC(38, 18) NOT NULL,
    locked_after    NUMERIC(38, 18) NOT NULL,
    required_amount NUMERIC(38, 18) NOT NULL,
    spot_price      NUMERIC(38, 18) NOT NULL,
    reason          TEXT        NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fast lookup of a position's adjustment history and per-wallet audit queries.
CREATE INDEX IF NOT EXISTS idx_collateral_adjustments_position
    ON collateral_adjustments (position_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_collateral_adjustments_wallet
    ON collateral_adjustments (wallet_id, created_at DESC);
