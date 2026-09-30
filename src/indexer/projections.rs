//! Typed projection tables for decoded Zenith contract events.
//!
//! Each projection is written in the same DB transaction as the raw
//! `chain_events` row and the `indexer_cursors` update, so ingestion is
//! exactly-once: a crash before commit replays the batch, a crash after
//! commit resumes from the durable cursor.
//!
//! Projections are keyed by `(ledger, tx_hash, event_index)` so re-processing
//! a batch is idempotent via `ON CONFLICT DO NOTHING`.

use serde_json::Value;
use sqlx::{Postgres, Transaction};

use super::decoder::DecodedEvent;

/// A single typed projection row to persist alongside the raw event.
#[derive(Debug, Clone)]
pub struct ProjectionRow {
    /// Projection table name, e.g. `series_created`.
    pub table: &'static str,
    /// Column names in insertion order.
    pub columns: &'static [&'static str],
    /// Values matching `columns`.
    pub values: Vec<Value>,
}

/// Build the typed projection rows for a decoded event.
///
/// Unknown events return an empty vec: they are stored raw and counted in the
/// `indexer_unknown_events_total` metric, never crashing the indexer.
pub fn project(event: &DecodedEvent) -> Vec<ProjectionRow> {
    match event {
        DecodedEvent::SeriesCreated(e) => vec![ProjectionRow {
            table: "series_created",
            columns: &["series_id", "underlying", "strike", "expiry", "is_call"],
            values: vec![
                Value::String(e.series_id.clone()),
                Value::String(e.underlying.clone()),
                Value::String(e.strike.clone()),
                Value::Number(e.expiry.into()),
                Value::Bool(e.is_call),
            ],
        }],
        DecodedEvent::OptionMinted(e) => vec![ProjectionRow {
            table: "option_minted",
            columns: &["series_id", "holder", "amount"],
            values: vec![
                Value::String(e.series_id.clone()),
                Value::String(e.holder.clone()),
                Value::String(e.amount.clone()),
            ],
        }],
        DecodedEvent::OptionExercised(e) => vec![ProjectionRow {
            table: "option_exercised",
            columns: &["series_id", "holder", "amount", "payout"],
            values: vec![
                Value::String(e.series_id.clone()),
                Value::String(e.holder.clone()),
                Value::String(e.amount.clone()),
                Value::String(e.payout.clone()),
            ],
        }],
        DecodedEvent::CollateralDeposited(e) => vec![ProjectionRow {
            table: "collateral_deposited",
            columns: &["series_id", "depositor", "amount"],
            values: vec![
                Value::String(e.series_id.clone()),
                Value::String(e.depositor.clone()),
                Value::String(e.amount.clone()),
            ],
        }],
        DecodedEvent::Settled(e) => vec![ProjectionRow {
            table: "settled",
            columns: &["series_id", "settlement_price"],
            values: vec![
                Value::String(e.series_id.clone()),
                Value::String(e.settlement_price.clone()),
            ],
        }],
        DecodedEvent::Unknown { .. } => Vec::new(),
    }
}

/// Persist all projection rows for a decoded event inside the caller's
/// transaction. Idempotent on `(ledger, tx_hash, event_index)`.
pub async fn persist(
    tx: &mut Transaction<'_, Postgres>,
    ledger: i64,
    tx_hash: &str,
    event_index: i32,
    event: &DecodedEvent,
) -> Result<(), sqlx::Error> {
    for row in project(event) {
        let mut cols: Vec<String> = vec![
            "ledger".into(),
            "tx_hash".into(),
            "event_index".into(),
        ];
        cols.extend(row.columns.iter().map(|c| (*c).to_string()));

        let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("${i}")).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT (ledger, tx_hash, event_index) DO NOTHING",
            row.table,
            cols.join(", "),
            placeholders.join(", "),
        );

        let mut q = sqlx::query(&sql)
            .bind(ledger)
            .bind(tx_hash)
            .bind(event_index);
        for value in &row.values {
            q = bind_json(q, value);
        }
        q.execute(&mut **tx).await?;
    }
    Ok(())
}

fn bind_json<'q>(
    q: sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments>,
    value: &'q Value,
) -> sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments> {
    match value {
        Value::String(s) => q.bind(s),
        Value::Bool(b) => q.bind(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                q.bind(i)
            } else {
                q.bind(n.to_string())
            }
        }
        other => q.bind(other.to_string()),
    }
}
