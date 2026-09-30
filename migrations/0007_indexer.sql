-- Migration: 0007_indexer
-- Contract event indexer for the Zenith Options Soroban contracts (issue #45).
--
-- Provides:
--   * chain_events          raw event log (ledger, tx hash, contract id, topic/data XDR, decoded JSON)
--   * indexer_cursors       durable per-stream cursor (stream, ledger, paging_token)
--   * typed projection tables for the known Zenith domain events
--
-- Exactly-once persistence is achieved by the indexer writing the events, their
-- projections and the cursor update inside a single transaction. The cursor row
-- is only advanced to the last event covered by that transaction, so a crash
-- before commit replays the batch and a crash after commit resumes past it.

BEGIN;

-- ---------------------------------------------------------------------------
-- Raw event log
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS chain_events (
    id              BIGSERIAL PRIMARY KEY,
    stream          TEXT        NOT NULL,
    ledger          BIGINT      NOT NULL,
    paging_token    TEXT        NOT NULL,
    tx_hash         TEXT        NOT NULL,
    contract_id     TEXT        NOT NULL,
    topic           TEXT[]      NOT NULL,
    data_xdr        TEXT        NOT NULL,
    decoded         JSONB,
    event_type      TEXT,
    in_successful_call BOOLEAN  NOT NULL DEFAULT TRUE,
    closed_at       TIMESTAMPTZ,
    ingested_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A paging token uniquely identifies an event within a stream, which makes
    -- re-ingestion idempotent even if a batch is replayed after a crash.
    CONSTRAINT chain_events_stream_paging_token_key UNIQUE (stream, paging_token)
);

CREATE INDEX IF NOT EXISTS chain_events_ledger_idx
    ON chain_events (ledger);
CREATE INDEX IF NOT EXISTS chain_events_contract_idx
    ON chain_events (contract_id, ledger);
CREATE INDEX IF NOT EXISTS chain_events_event_type_idx
    ON chain_events (event_type)
    WHERE event_type IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Durable cursor (one row per stream)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS indexer_cursors (
    stream       TEXT        PRIMARY KEY,
    ledger       BIGINT      NOT NULL,
    paging_token TEXT        NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Typed projections
-- ---------------------------------------------------------------------------

-- SeriesCreated: a new option series was deployed by the factory.
CREATE TABLE IF NOT EXISTS series_created (
    id            BIGSERIAL PRIMARY KEY,
    event_id      BIGINT      NOT NULL REFERENCES chain_events (id) ON DELETE CASCADE,
    ledger        BIGINT      NOT NULL,
    tx_hash       TEXT        NOT NULL,
    contract_id   TEXT        NOT NULL,
    series_id     TEXT        NOT NULL,
    underlying    TEXT,
    strike        NUMERIC(38, 0),
    expiry        TIMESTAMPTZ,
    is_call       BOOLEAN,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT series_created_event_id_key UNIQUE (event_id)
);

CREATE INDEX IF NOT EXISTS series_created_series_id_idx
    ON series_created (series_id);

-- OptionMinted: a writer minted option contracts against a series.
CREATE TABLE IF NOT EXISTS option_minted (
    id            BIGSERIAL PRIMARY KEY,
    event_id      BIGINT      NOT NULL REFERENCES chain_events (id) ON DELETE CASCADE,
    ledger        BIGINT      NOT NULL,
    tx_hash       TEXT        NOT NULL,
    contract_id   TEXT        NOT NULL,
    series_id     TEXT        NOT NULL,
    holder        TEXT,
    amount        NUMERIC(38, 0),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT option_minted_event_id_key UNIQUE (event_id)
);

CREATE INDEX IF NOT EXISTS option_minted_series_id_idx
    ON option_minted (series_id);
CREATE INDEX IF NOT EXISTS option_minted_holder_idx
    ON option_minted (holder);

-- OptionExercised: a holder exercised an option position.
CREATE TABLE IF NOT EXISTS option_exercised (
    id            BIGSERIAL PRIMARY KEY,
    event_id      BIGINT      NOT NULL REFERENCES chain_events (id) ON DELETE CASCADE,
    ledger        BIGINT      NOT NULL,
    tx_hash       TEXT        NOT NULL,
    contract_id   TEXT        NOT NULL,
    series_id     TEXT        NOT NULL,
    holder        TEXT,
    amount        NUMERIC(38, 0),
    payout        NUMERIC(38, 0),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT option_exercised_event_id_key UNIQUE (event_id)
);

CREATE INDEX IF NOT EXISTS option_exercised_series_id_idx
    ON option_exercised (series_id);
CREATE INDEX IF NOT EXISTS option_exercised_holder_idx
    ON option_exercised (holder);

-- CollateralDeposited: collateral moved into a vault.
CREATE TABLE IF NOT EXISTS collateral_deposited (
    id            BIGSERIAL PRIMARY KEY,
    event_id      BIGINT      NOT NULL REFERENCES chain_events (id) ON DELETE CASCADE,
    ledger        BIGINT      NOT NULL,
    tx_hash       TEXT        NOT NULL,
    contract_id   TEXT        NOT NULL,
    vault_id      TEXT,
    depositor     TEXT,
    asset         TEXT,
    amount        NUMERIC(38, 0),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT collateral_deposited_event_id_key UNIQUE (event_id)
);

CREATE INDEX IF NOT EXISTS collateral_deposited_vault_id_idx
    ON collateral_deposited (vault_id);
CREATE INDEX IF NOT EXISTS collateral_deposited_depositor_idx
    ON collateral_deposited (depositor);

-- Settled: a series or vault was settled.
CREATE TABLE IF NOT EXISTS settled (
    id            BIGSERIAL PRIMARY KEY,
    event_id      BIGINT      NOT NULL REFERENCES chain_events (id) ON DELETE CASCADE,
    ledger        BIGINT      NOT NULL,
    tx_hash       TEXT        NOT NULL,
    contract_id   TEXT        NOT NULL,
    series_id     TEXT,
    settlement_price NUMERIC(38, 0),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT settled_event_id_key UNIQUE (event_id)
);

CREATE INDEX IF NOT EXISTS settled_series_id_idx
    ON settled (series_id);

COMMIT;
