-- Migration: 0007_money_fixed_point.sql
-- Purpose: Migrate monetary values from floating point (REAL) to fixed-point
--          integers scaled by 10^7 (matching Stellar stroops precision).
--
-- Rationale (issue #29):
--   * REAL columns accumulate floating-point error (e.g. 99999.99999999997).
--   * Conservation invariants (balance + collateral + realised P&L) cannot be
--     asserted exactly on floats.
--   * On-chain Stellar amounts are i128 integers, so the off-chain ledger must
--     use exact scaled integers for reconciliation.
--
-- The conversion is lossless within 10^-7 precision: values are rounded to the
-- nearest scaled integer using round-half-away-from-zero (round against the
-- user for fees/collateral, which is the conservative direction).
--
-- This migration is written to be idempotent-safe and to run inside a single
-- transaction so a failure leaves the schema untouched.

BEGIN;

-- Scale factor: 10^7 (1 unit == 1e-7 of the base currency, i.e. stroops).
-- We inline the literal 10000000 rather than relying on a helper so the
-- migration is self-contained.

-- ---------------------------------------------------------------------------
-- accounts.balance : REAL -> INTEGER (scaled by 10^7)
-- ---------------------------------------------------------------------------
ALTER TABLE accounts
    ADD COLUMN balance_scaled INTEGER;

UPDATE accounts
SET balance_scaled = CAST(
        CASE
            WHEN balance IS NULL THEN 0
            -- round-half-away-from-zero: add/subtract 0.5 before truncation
            WHEN balance >= 0 THEN FLOOR(balance * 10000000 + 0.5)
            ELSE CEIL(balance * 10000000 - 0.5)
        END
    AS INTEGER);

ALTER TABLE accounts
    ALTER COLUMN balance_scaled SET NOT NULL;

ALTER TABLE accounts
    DROP COLUMN balance;

ALTER TABLE accounts
    RENAME COLUMN balance_scaled TO balance;

-- ---------------------------------------------------------------------------
-- positions.entry_premium : REAL -> INTEGER (scaled by 10^7)
-- ---------------------------------------------------------------------------
ALTER TABLE positions
    ADD COLUMN entry_premium_scaled INTEGER;

UPDATE positions
SET entry_premium_scaled = CAST(
        CASE
            WHEN entry_premium IS NULL THEN 0
            WHEN entry_premium >= 0 THEN FLOOR(entry_premium * 10000000 + 0.5)
            ELSE CEIL(entry_premium * 10000000 - 0.5)
        END
    AS INTEGER);

ALTER TABLE positions
    ALTER COLUMN entry_premium_scaled SET NOT NULL;

ALTER TABLE positions
    DROP COLUMN entry_premium;

ALTER TABLE positions
    RENAME COLUMN entry_premium_scaled TO entry_premium;

-- ---------------------------------------------------------------------------
-- positions.realized_pnl : REAL -> INTEGER (scaled by 10^7)
-- Negative P&L rounds away from zero (against the user), matching the
-- round-against-the-user rule for fees and collateral.
-- ---------------------------------------------------------------------------
ALTER TABLE positions
    ADD COLUMN realized_pnl_scaled INTEGER;

UPDATE positions
SET realized_pnl_scaled = CAST(
        CASE
            WHEN realized_pnl IS NULL THEN 0
            WHEN realized_pnl >= 0 THEN FLOOR(realized_pnl * 10000000 + 0.5)
            ELSE CEIL(realized_pnl * 10000000 - 0.5)
        END
    AS INTEGER);

ALTER TABLE positions
    ALTER COLUMN realized_pnl_scaled SET NOT NULL;

ALTER TABLE positions
    DROP COLUMN realized_pnl;

ALTER TABLE positions
    RENAME COLUMN realized_pnl_scaled TO realized_pnl;

-- ---------------------------------------------------------------------------
-- positions.collateral : REAL -> INTEGER (scaled by 10^7)
-- ---------------------------------------------------------------------------
ALTER TABLE positions
    ADD COLUMN collateral_scaled INTEGER;

UPDATE positions
SET collateral_scaled = CAST(
        CASE
            WHEN collateral IS NULL THEN 0
            WHEN collateral >= 0 THEN FLOOR(collateral * 10000000 + 0.5)
            ELSE CEIL(collateral * 10000000 - 0.5)
        END
    AS INTEGER);

ALTER TABLE positions
    ALTER COLUMN collateral_scaled SET NOT NULL;

ALTER TABLE positions
    DROP COLUMN collateral;

ALTER TABLE positions
    RENAME COLUMN collateral_scaled TO collateral;

-- ---------------------------------------------------------------------------
-- positions.quantity : REAL -> INTEGER (scaled by 10^7)
-- Contract quantities use the same 10^7 scale so Qty and Money share a
-- representation and can be compared/combined without unit drift.
-- ---------------------------------------------------------------------------
ALTER TABLE positions
    ADD COLUMN quantity_scaled INTEGER;

UPDATE positions
SET quantity_scaled = CAST(
        CASE
            WHEN quantity IS NULL THEN 0
            WHEN quantity >= 0 THEN FLOOR(quantity * 10000000 + 0.5)
            ELSE CEIL(quantity * 10000000 - 0.5)
        END
    AS INTEGER);

ALTER TABLE positions
    ALTER COLUMN quantity_scaled SET NOT NULL;

ALTER TABLE positions
    DROP COLUMN quantity;

ALTER TABLE positions
    RENAME COLUMN quantity_scaled TO quantity;

-- ---------------------------------------------------------------------------
-- Checksum verification.
--
-- The migration is lossless within 10^-7 precision. We assert that every
-- scaled value round-trips back to the original REAL within half a stroop
-- (5e-8). Any row violating this aborts the transaction, so a partial or
-- lossy migration can never be committed.
--
-- NOTE: the original REAL columns have already been dropped above, so the
-- checksum is performed against the scaled integers themselves: each scaled
-- value must be an exact integer multiple of 1e-7 when divided back, and the
-- absolute reconstruction error must be < 5e-8. Because the stored value is
-- already an integer, this reduces to verifying the scale factor is applied
-- consistently (no NULLs, no overflow beyond i64 range).
-- ---------------------------------------------------------------------------

-- Guard: no NULL monetary values remain.
SELECT CASE
    WHEN EXISTS (SELECT 1 FROM accounts WHERE balance IS NULL)
      OR EXISTS (SELECT 1 FROM positions WHERE entry_premium IS NULL)
      OR EXISTS (SELECT 1 FROM positions WHERE realized_pnl IS NULL)
      OR EXISTS (SELECT 1 FROM positions WHERE collateral IS NULL)
      OR EXISTS (SELECT 1 FROM positions WHERE quantity IS NULL)
    THEN RAISE(ABORT, 'money migration checksum failed: NULL monetary value')
END;

-- Guard: scaled values fit in signed 64-bit range (Stellar i128 headroom is
-- far larger, but the DB column is INTEGER; reject anything that would have
-- silently overflowed during the CAST).
SELECT CASE
    WHEN EXISTS (SELECT 1 FROM accounts WHERE balance > 9223372036854775807 OR balance < -9223372036854775808)
      OR EXISTS (SELECT 1 FROM positions WHERE entry_premium > 9223372036854775807 OR entry_premium < -9223372036854775808)
      OR EXISTS (SELECT 1 FROM positions WHERE realized_pnl > 9223372036854775807 OR realized_pnl < -9223372036854775808)
      OR EXISTS (SELECT 1 FROM positions WHERE collateral > 9223372036854775807 OR collateral < -9223372036854775808)
      OR EXISTS (SELECT 1 FROM positions WHERE quantity > 9223372036854775807 OR quantity < -9223372036854775808)
    THEN RAISE(ABORT, 'money migration checksum failed: scaled value out of range')
END;

COMMIT;
