-- Notifications inbox: lifecycle notices for positions (expiry, settlement, assignment, collateral).
-- Dedupe key is (wallet, kind, subject_id) so a given notice fires at most once per subject.

CREATE TABLE IF NOT EXISTS notifications (
    id          BIGSERIAL PRIMARY KEY,
    wallet      TEXT        NOT NULL,
    kind        TEXT        NOT NULL,
    subject_id  TEXT        NOT NULL,
    payload     JSONB       NOT NULL DEFAULT '{}'::jsonb,
    read_at     TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT notifications_wallet_kind_subject_key
        UNIQUE (wallet, kind, subject_id)
);

-- Cursor pagination: newest first, keyed by (created_at, id).
CREATE INDEX IF NOT EXISTS notifications_wallet_created_idx
    ON notifications (wallet, created_at DESC, id DESC);

-- Unread lookups for the inbox badge / mark-all-as-read.
CREATE INDEX IF NOT EXISTS notifications_wallet_unread_idx
    ON notifications (wallet, created_at DESC)
    WHERE read_at IS NULL;

-- Per-wallet notification preferences (which kinds the wallet wants delivered).
CREATE TABLE IF NOT EXISTS notification_preferences (
    wallet      TEXT        PRIMARY KEY,
    kinds       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
