-- Double-entry account ledger (issue #30)
--
-- ledger_accounts enumerates the chart of accounts. ledger_entries stores
-- balanced journal postings: every journal_id must sum to zero, enforced by
-- a deferred constraint trigger so multi-row inserts inside one transaction
-- are validated at COMMIT time.

CREATE TABLE IF NOT EXISTS ledger_accounts (
    id          BIGSERIAL PRIMARY KEY,
    code        TEXT NOT NULL UNIQUE,
    kind        TEXT NOT NULL CHECK (kind IN (
                    'user_cash',
                    'user_collateral',
                    'protocol_fees',
                    'insurance_fund',
                    'platform_counterparty'
                )),
    wallet      TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A user-scoped account must carry a wallet; protocol accounts must not.
    CONSTRAINT ledger_accounts_wallet_scope CHECK (
        (kind IN ('user_cash', 'user_collateral') AND wallet IS NOT NULL)
        OR (kind IN ('protocol_fees', 'insurance_fund', 'platform_counterparty') AND wallet IS NULL)
    )
);

-- One cash and one collateral account per wallet.
CREATE UNIQUE INDEX IF NOT EXISTS ledger_accounts_user_unique
    ON ledger_accounts (kind, wallet)
    WHERE wallet IS NOT NULL;

-- Singleton protocol accounts (one row per kind).
CREATE UNIQUE INDEX IF NOT EXISTS ledger_accounts_protocol_unique
    ON ledger_accounts (kind)
    WHERE wallet IS NULL;

CREATE TABLE IF NOT EXISTS ledger_entries (
    id          BIGSERIAL PRIMARY KEY,
    journal_id  UUID NOT NULL,
    account_id  BIGINT NOT NULL REFERENCES ledger_accounts (id),
    -- Signed amount in the smallest unit; positive = debit, negative = credit.
    amount      BIGINT NOT NULL,
    -- Optional linkage back to the domain event that produced the posting.
    ref_type    TEXT,
    ref_id      TEXT,
    memo        TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ledger_entries_journal_idx
    ON ledger_entries (journal_id);

CREATE INDEX IF NOT EXISTS ledger_entries_account_idx
    ON ledger_entries (account_id, id DESC);

-- Paginated wallet history: join entries to the wallet's accounts.
CREATE INDEX IF NOT EXISTS ledger_entries_account_created_idx
    ON ledger_entries (account_id, created_at DESC, id DESC);

-- DB-enforced balance guarantee: each journal must sum to zero. Implemented as
-- a DEFERRABLE INITIALLY DEFERRED constraint trigger so all postings of a
-- journal can be inserted within the same transaction and are checked once at
-- COMMIT. A journal with a single unbalanced posting is rejected.
CREATE OR REPLACE FUNCTION ledger_assert_journal_balanced()
RETURNS TRIGGER AS $$
DECLARE
    total BIGINT;
BEGIN
    SELECT COALESCE(SUM(amount), 0)
      INTO total
      FROM ledger_entries
     WHERE journal_id = NEW.journal_id;

    IF total <> 0 THEN
        RAISE EXCEPTION
            'unbalanced journal %: sum(amount) = % (must be 0)',
            NEW.journal_id, total
            USING ERRCODE = 'check_violation';
    END IF;

    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS ledger_entries_balanced ON ledger_entries;
CREATE CONSTRAINT TRIGGER ledger_entries_balanced
    AFTER INSERT OR UPDATE ON ledger_entries
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION ledger_assert_journal_balanced();

-- Seed the singleton protocol accounts so application code can look them up
-- by kind without racing on first use.
INSERT INTO ledger_accounts (code, kind)
VALUES
    ('PROTOCOL_FEES',        'protocol_fees'),
    ('INSURANCE_FUND',       'insurance_fund'),
    ('PLATFORM_COUNTERPARTY','platform_counterparty')
ON CONFLICT (code) DO NOTHING;
