-- Migration: 0007_liquidations.sql
-- Purpose: Persist liquidation events for the risk engine (issue #25).
-- Records the account, the positions closed during a liquidation step, the
-- prices used, the penalty credited to the insurance fund, and the pre/post
-- health figures so the risk engine can be audited and replayed.

CREATE TABLE IF NOT EXISTS liquidations (
    id                  BIGSERIAL PRIMARY KEY,
    account_id          BIGINT      NOT NULL REFERENCES accounts (id),
    -- Positions closed in this liquidation step, in the order they were closed.
    -- Each entry: { position_id, symbol, quantity, price, maintenance_margin_before }.
    positions_closed    JSONB       NOT NULL DEFAULT '[]'::jsonb,
    -- Prices used to value the closed positions, keyed by symbol.
    prices              JSONB       NOT NULL DEFAULT '{}'::jsonb,
    -- Liquidation penalty in basis points applied to this step.
    penalty_bps         INTEGER     NOT NULL,
    -- Penalty amount credited to the insurance-fund ledger account.
    penalty_amount      NUMERIC(38, 18) NOT NULL DEFAULT 0,
    -- Health figures captured before and after the step.
    equity_before       NUMERIC(38, 18) NOT NULL,
    equity_after        NUMERIC(38, 18) NOT NULL,
    maintenance_before  NUMERIC(38, 18) NOT NULL,
    maintenance_after   NUMERIC(38, 18) NOT NULL,
    -- Bad debt recorded when equity goes negative after full liquidation;
    -- absorbed by the insurance fund.
    bad_debt            NUMERIC(38, 18) NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fast lookup of an account's liquidation history, newest first.
CREATE INDEX IF NOT EXISTS idx_liquidations_account_created
    ON liquidations (account_id, created_at DESC);

-- Support scanning recent liquidations across all accounts.
CREATE INDEX IF NOT EXISTS idx_liquidations_created
    ON liquidations (created_at DESC);
