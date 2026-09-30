-- 0007: database-level invariants (CHECK constraints + integrity triggers).
--
-- Fund-safety invariants are enforced here as defence in depth, not only in
-- application code: non-negative balance and collateral, positive
-- contracts/strike, status/column coherence, legal status transitions, and
-- immutability of the columns a position is born with.
--
-- SQLite cannot add CHECK constraints to an existing table, so the two tables
-- are rebuilt. Existing data is validated first; the migration aborts with a
-- report if any row would violate a new constraint.

-- ── 1. validate existing data ───────────────────────────────────────────────
-- A temp trigger is the only way to conditionally abort a migration script:
-- RAISE() is valid only inside a trigger. Each SELECT aborts with a report if
-- its constraint is already violated by existing rows.
DROP TABLE IF EXISTS _zenith_m0007_check;
CREATE TEMP TABLE _zenith_m0007_check (x);
CREATE TEMP TRIGGER _zenith_m0007_validate
BEFORE INSERT ON _zenith_m0007_check
FOR EACH ROW
BEGIN
    SELECT CASE WHEN EXISTS (SELECT 1 FROM accounts WHERE balance < 0)
        THEN RAISE(ABORT, '0007: accounts with balance < 0') END;
    SELECT CASE WHEN EXISTS (SELECT 1 FROM accounts WHERE collateral_locked < 0)
        THEN RAISE(ABORT, '0007: accounts with collateral_locked < 0') END;
    SELECT CASE WHEN EXISTS (SELECT 1 FROM positions WHERE contracts <= 0)
        THEN RAISE(ABORT, '0007: positions with contracts <= 0') END;
    SELECT CASE WHEN EXISTS (SELECT 1 FROM positions WHERE strike <= 0)
        THEN RAISE(ABORT, '0007: positions with strike <= 0') END;
    SELECT CASE WHEN EXISTS (
        SELECT 1 FROM positions
        WHERE (status = 'open' AND (close_premium IS NOT NULL OR close_spot IS NOT NULL OR realized_pnl IS NOT NULL OR closed_at IS NOT NULL))
           OR (status IN ('closed', 'rolled') AND (close_premium IS NULL OR close_spot IS NULL OR realized_pnl IS NULL OR closed_at IS NULL))
    ) THEN RAISE(ABORT, '0007: positions with status/column incoherence') END;
END;
INSERT INTO _zenith_m0007_check VALUES (1);
DROP TABLE _zenith_m0007_check;

-- ── 2. rebuild accounts with CHECK constraints ──────────────────────────────
-- Column definitions are unchanged (same names, types, NOT NULL flags and
-- defaults); only the two CHECK constraints are added.
CREATE TABLE accounts_new (
    wallet_address    TEXT PRIMARY KEY,
    balance           REAL NOT NULL DEFAULT 100000.0 CHECK (balance >= 0),
    collateral_locked REAL NOT NULL DEFAULT 0.0 CHECK (collateral_locked >= 0),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
INSERT INTO accounts_new SELECT * FROM accounts;
DROP TABLE accounts;
ALTER TABLE accounts_new RENAME TO accounts;

-- ── 3. rebuild positions with CHECK constraints ─────────────────────────────
CREATE TABLE positions_new (
    id              TEXT PRIMARY KEY,
    wallet_address  TEXT NOT NULL REFERENCES accounts(wallet_address),
    underlying      TEXT NOT NULL,
    strike          REAL NOT NULL CHECK (strike > 0),
    expiry_days     REAL NOT NULL,
    option_type     TEXT NOT NULL CHECK (option_type IN ('call', 'put')),
    position_type   TEXT NOT NULL CHECK (position_type IN ('long', 'short')),
    contracts       REAL NOT NULL CHECK (contracts > 0),
    entry_premium   REAL NOT NULL,
    entry_spot      REAL NOT NULL,
    collateral      REAL NOT NULL DEFAULT 0.0,
    status          TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed', 'rolled')),
    close_premium   REAL,
    close_spot      REAL,
    realized_pnl    REAL,
    opened_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    closed_at       TEXT,
    strategy_id     TEXT,
    -- status/column coherence: an open position has no settlement columns;
    -- a closed/rolled one must have all of them.
    CHECK (
        (status = 'open' AND close_premium IS NULL AND close_spot IS NULL AND realized_pnl IS NULL AND closed_at IS NULL)
        OR (status IN ('closed', 'rolled') AND close_premium IS NOT NULL AND close_spot IS NOT NULL AND realized_pnl IS NOT NULL AND closed_at IS NOT NULL)
    )
);
INSERT INTO positions_new SELECT * FROM positions;
DROP TABLE positions;
ALTER TABLE positions_new RENAME TO positions;

CREATE INDEX idx_positions_wallet ON positions(wallet_address);
CREATE INDEX idx_positions_wallet_status ON positions(wallet_address, status);
CREATE INDEX idx_positions_strategy ON positions(strategy_id) WHERE strategy_id IS NOT NULL;

-- ── 4. integrity triggers ───────────────────────────────────────────────────
-- Reject illegal status transitions. A position is born 'open' and may go to
-- 'closed' or 'rolled'; a roll relabels 'closed' to 'rolled'. Anything else
-- (e.g. closed -> open, rolled -> closed) is rejected.
CREATE TRIGGER positions_status_transition
BEFORE UPDATE OF status ON positions
FOR EACH ROW
WHEN OLD.status <> NEW.status
 AND NOT (
        (OLD.status = 'open' AND NEW.status IN ('closed', 'rolled'))
        OR (OLD.status = 'closed' AND NEW.status = 'rolled')
    )
BEGIN
    SELECT RAISE(ABORT, 'illegal position status transition');
END;

-- Forbid UPDATEs to the columns a position is born with. The close/roll
-- settlements only ever write status and the settlement columns, so this
-- never fires for the application's own writes.
CREATE TRIGGER positions_immutable_columns
BEFORE UPDATE ON positions
FOR EACH ROW
WHEN NEW.entry_premium IS NOT OLD.entry_premium
  OR NEW.opened_at IS NOT OLD.opened_at
  OR NEW.wallet_address IS NOT OLD.wallet_address
BEGIN
    SELECT RAISE(ABORT, 'position columns entry_premium, opened_at and wallet_address are immutable');
END;
