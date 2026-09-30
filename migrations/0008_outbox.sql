-- Transactional outbox for domain events. Events are written into this
-- table inside the same transaction as the state change that produced
-- them (see the emit() calls in positions.rs, strategies.rs, alerts.rs),
-- so a rolled-back transaction never emits an event and a crash after
-- commit never loses one. The relay in src/outbox.rs polls unpublished
-- rows in id order and dispatches them to in-process consumers.
--
-- `id` is monotonic (AUTOINCREMENT) so "next batch" is always
-- `id > last seen`, and `published_at` stays NULL until every registered
-- consumer has recorded an offset past the event.

CREATE TABLE outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    aggregate_type  TEXT NOT NULL,
    aggregate_id    TEXT NOT NULL,
    event_type      TEXT NOT NULL,
    payload         TEXT NOT NULL, -- versioned JSON, see DomainEvent
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    published_at    TEXT
);

-- The relay's poll query: oldest unpublished events first.
CREATE INDEX idx_outbox_unpublished ON outbox(id) WHERE published_at IS NULL;
-- The retention prune: published events older than the retention window.
CREATE INDEX idx_outbox_published ON outbox(published_at) WHERE published_at IS NOT NULL;

-- Per-consumer progress: the last outbox id each consumer has handled. A
-- slow or failing consumer's offset simply doesn't advance, so it retries
-- the same events next poll without holding up the others.
CREATE TABLE outbox_consumer_offsets (
    consumer      TEXT PRIMARY KEY,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Idempotency records for consumers: at-least-once delivery means a
-- consumer can see the same event twice (e.g. the relay retried after a
-- crash between dispatch and offset update), so consumers mark each
-- handled event here and skip ones they've already seen. Keyed by
-- (consumer, event_id).
CREATE TABLE outbox_dedupe (
    consumer     TEXT NOT NULL,
    event_id     INTEGER NOT NULL,
    processed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (consumer, event_id)
);
