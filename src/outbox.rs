//! Transactional outbox for domain events.
//!
//! State changes (position open/close, strategy execution, alert triggers)
//! write a row into the `outbox` table *inside the same transaction* as the
//! change itself via [`emit`], so a rolled-back transaction never emits an
//! event and a crash after commit never loses one. The [`OutboxRelay`]
//! polls unpublished rows in id order and dispatches them to in-process
//! consumers with at-least-once delivery and per-aggregate ordering (the
//! monotonic `id` defines a total order per aggregate).
//!
//! Consumers must be idempotent: at-least-once means a consumer can see the
//! same event twice (e.g. the relay retried after a crash between dispatch
//! and offset update), so [`is_event_processed`] / [`mark_event_processed`]
//! provide a dedupe helper keyed by event id.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::Sqlite;
use sqlx::{FromRow, SqlitePool, Transaction};

use crate::models::Position;

/// Version stamped into every event payload. Consumers can branch on it as
/// payloads evolve; the relay ignores it (it dispatches whatever it finds).
pub const EVENT_VERSION: u32 = 1;

// ─── Versioned payloads ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionOpenedPayload {
    pub version: u32,
    pub position_id: String,
    pub wallet_address: String,
    pub underlying: String,
    pub strike: f64,
    pub expiry_days: f64,
    pub option_type: String,
    pub position_type: String,
    pub contracts: f64,
    pub entry_premium: f64,
    pub entry_spot: f64,
    pub collateral: f64,
    pub strategy_id: Option<String>,
    pub opened_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionClosedPayload {
    pub version: u32,
    pub position_id: String,
    pub wallet_address: String,
    pub underlying: String,
    pub strike: f64,
    pub contracts: f64,
    pub entry_premium: f64,
    pub close_premium: f64,
    pub close_spot: f64,
    pub realized_pnl: f64,
    pub closed_at: String,
    pub strategy_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyExecutedPayload {
    pub version: u32,
    pub strategy_id: String,
    pub wallet_address: String,
    pub underlying: String,
    pub leg_ids: Vec<String>,
    pub opened_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertTriggeredPayload {
    pub version: u32,
    pub alert_id: String,
    pub wallet_address: String,
    pub underlying: String,
    pub condition: String,
    pub target_price: f64,
    pub triggered_at: String,
}

/// Defined for completeness as part of the domain event vocabulary. There is
/// no separate settlement flow in the app yet (a position close *is* its
/// settlement), so nothing emits this today.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettledPayload {
    pub version: u32,
    pub position_id: String,
    pub wallet_address: String,
    pub realized_pnl: f64,
    pub settled_at: String,
}

/// Defined for completeness. The app has no liquidation flow, so nothing
/// emits this today.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidatedPayload {
    pub version: u32,
    pub position_id: String,
    pub wallet_address: String,
    pub collateral_liquidated: f64,
    pub liquidated_at: String,
}

/// The typed domain-event vocabulary. Serialized into the outbox `payload`
/// column with the event type as a tag and the schema version alongside the
/// event-specific fields, e.g.
/// `{"type":"position_opened","version":1,"position_id":"...", ...}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainEvent {
    PositionOpened(PositionOpenedPayload),
    PositionClosed(PositionClosedPayload),
    StrategyExecuted(StrategyExecutedPayload),
    AlertTriggered(AlertTriggeredPayload),
    Settled(SettledPayload),
    Liquidated(LiquidatedPayload),
}

impl DomainEvent {
    /// (aggregate_type, aggregate_id, event_type) for the outbox columns.
    /// The aggregate id is what gives per-aggregate ordering: the relay
    /// polls in global id order, and consumers that need per-aggregate
    /// ordering can rely on an aggregate's events arriving in id order.
    pub fn classify(&self) -> (&'static str, &str, &'static str) {
        match self {
            DomainEvent::PositionOpened(p) => ("position", &p.position_id, "position_opened"),
            DomainEvent::PositionClosed(p) => ("position", &p.position_id, "position_closed"),
            DomainEvent::StrategyExecuted(p) => ("strategy", &p.strategy_id, "strategy_executed"),
            DomainEvent::AlertTriggered(p) => ("alert", &p.alert_id, "alert_triggered"),
            DomainEvent::Settled(p) => ("position", &p.position_id, "settled"),
            DomainEvent::Liquidated(p) => ("position", &p.position_id, "liquidated"),
        }
    }

    pub fn position_opened(p: &Position) -> Self {
        DomainEvent::PositionOpened(PositionOpenedPayload {
            version: EVENT_VERSION,
            position_id: p.id.clone(),
            wallet_address: p.wallet_address.clone(),
            underlying: p.underlying.clone(),
            strike: p.strike,
            expiry_days: p.expiry_days,
            option_type: p.option_type.clone(),
            position_type: p.position_type.clone(),
            contracts: p.contracts,
            entry_premium: p.entry_premium,
            entry_spot: p.entry_spot,
            collateral: p.collateral,
            strategy_id: p.strategy_id.clone(),
            opened_at: p.opened_at.clone(),
        })
    }

    pub fn position_closed(p: &Position) -> Self {
        DomainEvent::PositionClosed(PositionClosedPayload {
            version: EVENT_VERSION,
            position_id: p.id.clone(),
            wallet_address: p.wallet_address.clone(),
            underlying: p.underlying.clone(),
            strike: p.strike,
            contracts: p.contracts,
            entry_premium: p.entry_premium,
            close_premium: p.close_premium.unwrap_or(0.0),
            close_spot: p.close_spot.unwrap_or(0.0),
            realized_pnl: p.realized_pnl.unwrap_or(0.0),
            closed_at: p.closed_at.clone().unwrap_or_default(),
            strategy_id: p.strategy_id.clone(),
        })
    }
}

// ─── Emission ────────────────────────────────────────────────────────────────

/// Writes one event into the outbox. MUST be called inside the same
/// transaction as the state change that produced the event — that shared
/// transaction is the whole pattern: the event commits iff the state change
/// does, and rolls back iff it does.
pub async fn emit(tx: &mut Transaction<'_, Sqlite>, event: &DomainEvent) -> Result<(), sqlx::Error> {
    let (aggregate_type, aggregate_id, event_type) = event.classify();
    let payload =
        serde_json::to_string(event).expect("DomainEvent is always serializable to JSON");
    sqlx::query(
        "INSERT INTO outbox (aggregate_type, aggregate_id, event_type, payload)
         VALUES (?, ?, ?, ?)",
    )
    .bind(aggregate_type)
    .bind(aggregate_id)
    .bind(event_type)
    .bind(payload)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ─── Consumers ───────────────────────────────────────────────────────────────

/// An in-process outbox consumer. Implementations must be idempotent (see
/// the module docs and the dedupe helpers).
#[async_trait]
pub trait OutboxConsumer: Send + Sync {
    fn name(&self) -> &str;
    async fn handle(
        &self,
        event: &DomainEvent,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// Built-in consumer that logs every event it receives. Registered by
/// default so a running backend always has at least one consumer and the
/// relay's progress is visible in the logs.
pub struct LoggingConsumer;

#[async_trait]
impl OutboxConsumer for LoggingConsumer {
    fn name(&self) -> &str {
        "logging"
    }

    async fn handle(
        &self,
        event: &DomainEvent,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!(event = ?event, "outbox event dispatched");
        Ok(())
    }
}

// ─── Relay ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, FromRow)]
struct OutboxRow {
    id: i64,
    payload: String,
}

/// Polls the outbox in id order and dispatches each batch to its registered
/// consumers, recording a per-consumer offset so a slow or failing consumer
/// retries without holding up the others.
pub struct OutboxRelay {
    db: SqlitePool,
    consumers: Vec<std::sync::Arc<dyn OutboxConsumer>>,
    poll_interval: std::time::Duration,
    batch_size: i64,
    retention: std::time::Duration,
}

impl OutboxRelay {
    pub fn new(db: SqlitePool) -> Self {
        Self {
            db,
            consumers: Vec::new(),
            poll_interval: std::time::Duration::from_secs(1),
            batch_size: 100,
            retention: std::time::Duration::from_secs(7 * 24 * 60 * 60),
        }
    }

    pub fn add_consumer(&mut self, consumer: std::sync::Arc<dyn OutboxConsumer>) {
        self.consumers.push(consumer);
    }

    /// Polls and dispatches one batch of unpublished events, then prunes
    /// published events past the retention window. Returns the number of
    /// events dispatched. Exposed separately from [`run_loop`] so tests can
    /// drive the relay deterministically instead of waiting on a timer.
    pub async fn relay_once(&self) -> Result<u64, sqlx::Error> {
        self.replay_for_new_consumers().await?;

        let events: Vec<OutboxRow> = sqlx::query_as(
            "SELECT id, payload FROM outbox
              WHERE published_at IS NULL
             ORDER BY id
             LIMIT ?",
        )
        .bind(self.batch_size)
        .fetch_all(&self.db)
        .await?;

        let mut dispatched = 0;
        for row in &events {
            let event: DomainEvent = match serde_json::from_str(&row.payload) {
                Ok(event) => event,
                Err(e) => {
                    // A payload that can't be parsed would otherwise block
                    // every later event forever (it's always the oldest
                    // unpublished row). Mark it published so the relay moves
                    // on; the corruption is logged for a human to find.
                    tracing::error!(error = %e, event_id = row.id, "outbox payload failed to parse; marking published");
                    sqlx::query("UPDATE outbox SET published_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
                        .bind(row.id)
                        .execute(&self.db)
                        .await?;
                    continue;
                }
            };

            // Dispatch to every consumer concurrently: a slow consumer must
            // not block the others from making progress on the same event.
            let mut set = tokio::task::JoinSet::new();
            for consumer in &self.consumers {
                let consumer = std::sync::Arc::clone(consumer);
                let event = event.clone();
                set.spawn(async move {
                    let result = consumer.handle(&event).await;
                    (consumer.name().to_string(), result)
                });
            }
            let mut all_ok = true;
            while let Some(joined) = set.join_next().await {
                let (name, result) = joined.expect("consumer task panicked");
                match result {
                    Ok(()) => {
                        self.record_offset(&name, row.id).await?;
                    }
                    Err(e) => {
                        all_ok = false;
                        tracing::warn!(error = %e, consumer = name, event_id = row.id, "outbox consumer failed; event stays unpublished and will retry");
                    }
                }
            }

            // Only mark the event published once every consumer's offset is
            // past it — until then it stays unpublished and the next poll
            // redelivers it (consumers are idempotent).
            if all_ok && !self.consumers.is_empty() {
                sqlx::query("UPDATE outbox SET published_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
                    .bind(row.id)
                    .execute(&self.db)
                    .await?;
            }
            dispatched += 1;
        }

        self.prune_published().await?;
        Ok(dispatched)
    }

    /// Records that `consumer` has successfully handled `event_id`.
    async fn record_offset(&self, consumer: &str, event_id: i64) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO outbox_consumer_offsets (consumer, last_event_id, updated_at)
             VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ON CONFLICT(consumer) DO UPDATE SET
                last_event_id = excluded.last_event_id,
                updated_at = excluded.updated_at",
        )
        .bind(consumer)
        .bind(event_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// A consumer that has never handled an event (offset 0) must receive
    /// every event, including ones already published to earlier consumers.
    /// Reset those rows' published_at so the relay redelivers them; the
    /// dedupe table keeps the redelivery idempotent.
    async fn replay_for_new_consumers(&self) -> Result<(), sqlx::Error> {
        let has_published: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM outbox WHERE published_at IS NOT NULL)",
        )
        .fetch_one(&self.db)
        .await?;
        if !has_published {
            return Ok(());
        }
        for consumer in &self.consumers {
            let offset: Option<i64> = sqlx::query_scalar(
                "SELECT last_event_id FROM outbox_consumer_offsets WHERE consumer = ?",
            )
            .bind(consumer.name())
            .fetch_optional(&self.db)
            .await?;
            if offset.unwrap_or(0) == 0 {
                sqlx::query(
                    "UPDATE outbox SET published_at = NULL WHERE published_at IS NOT NULL",
                )
                .execute(&self.db)
                .await?;
                break;
            }
        }
        Ok(())
    }

    /// Deletes published events older than the retention window. The cutoff
    /// is computed by SQLite in the same timestamp format the column stores,
    /// so the string comparison is a correct time comparison.
    pub async fn prune_published(&self) -> Result<u64, sqlx::Error> {
        let days = self.retention.as_secs() / 86400;
        let result = sqlx::query(
            "DELETE FROM outbox
              WHERE published_at IS NOT NULL
                AND published_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)",
        )
        .bind(format!("-{days} days"))
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected())
    }

    /// Polls forever. Runs as a background task spawned from `init_state`.
    pub async fn run_loop(&self) {
        let mut interval = tokio::time::interval(self.poll_interval);
        loop {
            interval.tick().await;
            match self.relay_once().await {
                Ok(n) if n > 0 => {
                    tracing::info!(events = n, "outbox relay dispatched a batch");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "outbox relay batch failed");
                }
            }
        }
    }
}

/// Builds a relay with the default logging consumer and runs it forever.
/// Spawned from `init_state` as a background task.
pub async fn relay_loop(db: SqlitePool) {
    let mut relay = OutboxRelay::new(db);
    relay.add_consumer(std::sync::Arc::new(LoggingConsumer));
    relay.run_loop().await;
}

// ─── Dedupe helpers ──────────────────────────────────────────────────────────

/// Returns true if `consumer` has already handled `event_id`. Consumers
/// call this before doing idempotent work.
pub async fn is_event_processed(
    db: &SqlitePool,
    consumer: &str,
    event_id: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM outbox_dedupe WHERE consumer = ? AND event_id = ?)",
    )
    .bind(consumer)
    .bind(event_id)
    .fetch_one(db)
    .await
}

/// Records that `consumer` has handled `event_id`. Consumers call this after
/// their idempotent work succeeds.
pub async fn mark_event_processed(
    db: &SqlitePool,
    consumer: &str,
    event_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO outbox_dedupe (consumer, event_id) VALUES (?, ?)")
        .bind(consumer)
        .bind(event_id)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn test_db() -> (SqlitePool, std::path::PathBuf) {
        let db_path =
            std::env::temp_dir().join(format!("zenith-outbox-test-{}.db", uuid::Uuid::new_v4()));
        let pool = crate::db::init_pool(&format!("sqlite://{}", db_path.display())).await;
        (pool, db_path)
    }

    async fn insert_event(db: &SqlitePool, event: &DomainEvent) {
        let mut tx = db.begin().await.unwrap();
        emit(&mut tx, event).await.unwrap();
        tx.commit().await.unwrap();
    }

    fn sample_event() -> DomainEvent {
        DomainEvent::PositionOpened(PositionOpenedPayload {
            version: EVENT_VERSION,
            position_id: "pos-1".into(),
            wallet_address: "W".into(),
            underlying: "BTC".into(),
            strike: 70000.0,
            expiry_days: 30.0,
            option_type: "call".into(),
            position_type: "long".into(),
            contracts: 1.0,
            entry_premium: 100.0,
            entry_spot: 67000.0,
            collateral: 0.0,
            strategy_id: None,
            opened_at: "2026-09-28T00:00:00.000Z".into(),
        })
    }

    /// A consumer that counts how many events it received and can be told to
    /// fail, so tests can exercise the retry and per-consumer-offset paths.
    struct TestConsumer {
        name: &'static str,
        received: AtomicUsize,
        fail: std::sync::atomic::AtomicBool,
    }

    impl TestConsumer {
        fn new(name: &'static str) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                name,
                received: AtomicUsize::new(0),
                fail: std::sync::atomic::AtomicBool::new(false),
            })
        }
    }

    #[async_trait]
    impl OutboxConsumer for TestConsumer {
        fn name(&self) -> &str {
            self.name
        }
        async fn handle(
            &self,
            _event: &DomainEvent,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.received.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err("boom".into());
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn relay_dispatches_events_in_id_order_and_marks_them_published() {
        let (db, db_path) = test_db().await;
        insert_event(&db, &sample_event()).await;
        insert_event(&db, &sample_event()).await;

        let consumer = TestConsumer::new("test");
        let relay = OutboxRelay {
            db: db.clone(),
            consumers: vec![consumer.clone()],
            poll_interval: std::time::Duration::from_millis(10),
            batch_size: 100,
            retention: std::time::Duration::from_secs(7 * 24 * 60 * 60),
        };

        let dispatched = relay.relay_once().await.unwrap();
        assert_eq!(dispatched, 2);
        assert_eq!(consumer.received.load(Ordering::SeqCst), 2);

        let published: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM outbox WHERE published_at IS NOT NULL")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(published, 2);

        let offset: i64 =
            sqlx::query_scalar("SELECT last_event_id FROM outbox_consumer_offsets WHERE consumer = 'test'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(offset, 2);

        // A second relay pass has nothing unpublished to do.
        let dispatched = relay.relay_once().await.unwrap();
        assert_eq!(dispatched, 0);

        db.close().await;
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn a_failing_consumer_does_not_block_the_others() {
        let (db, db_path) = test_db().await;
        insert_event(&db, &sample_event()).await;

        let good = TestConsumer::new("good");
        let bad = TestConsumer::new("bad");
        bad.fail.store(true, Ordering::SeqCst);

        let relay = OutboxRelay {
            db: db.clone(),
            consumers: vec![good.clone(), bad],
            poll_interval: std::time::Duration::from_millis(10),
            batch_size: 100,
            retention: std::time::Duration::from_secs(7 * 24 * 60 * 60),
        };

        relay.relay_once().await.unwrap();

        assert_eq!(good.received.load(Ordering::SeqCst), 1);
        // The good consumer's offset advanced past the event...
        let good_offset: i64 = sqlx::query_scalar(
            "SELECT last_event_id FROM outbox_consumer_offsets WHERE consumer = 'good'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(good_offset, 1);
        // ...but the event stays unpublished because the bad consumer hasn't
        // handled it, so it will be redelivered (and the good consumer's
        // dedupe keeps the redelivery idempotent).
        let unpublished: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM outbox WHERE published_at IS NULL")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(unpublished, 1);

        db.close().await;
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn dedupe_helpers_round_trip() {
        let (db, db_path) = test_db().await;
        assert!(!is_event_processed(&db, "c", 1).await.unwrap());
        mark_event_processed(&db, "c", 1).await.unwrap();
        assert!(is_event_processed(&db, "c", 1).await.unwrap());
        // Idempotent: marking twice doesn't error.
        mark_event_processed(&db, "c", 1).await.unwrap();
        assert!(is_event_processed(&db, "c", 1).await.unwrap());

        db.close().await;
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn prune_removes_published_events_past_retention() {
        let (db, db_path) = test_db().await;
        insert_event(&db, &sample_event()).await;

        // Backdate the event as published 8 days ago, past the 7-day window.
        sqlx::query(
            "UPDATE outbox SET published_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-8 days')",
        )
        .execute(&db)
        .await
        .unwrap();

        let relay = OutboxRelay::new(db.clone());
        let pruned = relay.prune_published().await.unwrap();
        assert_eq!(pruned, 1);

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM outbox")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(remaining, 0);

        db.close().await;
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn domain_event_serializes_with_type_tag_and_version() {
        let event = sample_event();
        let json = serde_json::to_string(&event).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "position_opened");
        assert_eq!(value["version"], 1);
        assert_eq!(value["position_id"], "pos-1");

        // And parses back into the typed enum.
        let parsed: DomainEvent = serde_json::from_str(&json).unwrap();
        let (aggregate_type, aggregate_id, event_type) = parsed.classify();
        assert_eq!(aggregate_type, "position");
        assert_eq!(aggregate_id, "pos-1");
        assert_eq!(event_type, "position_opened");
    }
}
