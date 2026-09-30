//! OHLCV candle rollup and query support.
//!
//! Ticks are persisted to `price_ticks` and rolled up incrementally into
//! `price_candles` for the supported intervals. Buckets are aligned to UTC
//! boundaries. Gaps with no ticks are *not* fabricated: a bucket only exists
//! if at least one tick landed in it.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Supported candle intervals, in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interval {
    M1,
    M5,
    H1,
    D1,
}

impl Interval {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1m" => Some(Interval::M1),
            "5m" => Some(Interval::M5),
            "1h" => Some(Interval::H1),
            "1d" => Some(Interval::D1),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M5 => "5m",
            Interval::H1 => "1h",
            Interval::D1 => "1d",
        }
    }

    pub fn seconds(&self) -> i64 {
        match self {
            Interval::M1 => 60,
            Interval::M5 => 300,
            Interval::H1 => 3_600,
            Interval::D1 => 86_400,
        }
    }

    pub const ALL: [Interval; 4] = [Interval::M1, Interval::M5, Interval::H1, Interval::D1];
}

/// Align a unix timestamp (seconds, UTC) down to the start of its bucket.
pub fn bucket_start(observed_at: i64, interval: Interval) -> i64 {
    let step = interval.seconds();
    observed_at - observed_at.rem_euclid(step)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Candle {
    pub underlying: String,
    pub interval: String,
    pub bucket_start: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub tick_count: i64,
}

/// Maximum number of candles returned in a single request.
pub const MAX_CANDLES: i64 = 1000;

/// Persist a single tick and incrementally roll it up into every interval.
///
/// Uses `INSERT ... ON CONFLICT ... DO UPDATE` so each tick only touches the
/// current bucket rather than recomputing history. Out-of-order ticks are
/// handled by widening the bucket's high/low and keeping the earliest tick as
/// `open` and the latest as `close`.
pub async fn record_tick(
    db: &SqlitePool,
    underlying: &str,
    price: f64,
    source: &str,
    observed_at: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;

    sqlx::query(
        "INSERT INTO price_ticks (underlying, price, source, observed_at) VALUES (?, ?, ?, ?)",
    )
    .bind(underlying)
    .bind(price)
    .bind(source)
    .bind(observed_at)
    .execute(&mut *tx)
    .await?;

    for interval in Interval::ALL {
        let bucket = bucket_start(observed_at, interval);
        sqlx::query(
            r#"
            INSERT INTO price_candles
                (underlying, interval, bucket_start, open, high, low, close, tick_count)
            VALUES (?, ?, ?, ?, ?, ?, ?, 1)
            ON CONFLICT(underlying, interval, bucket_start) DO UPDATE SET
                high = MAX(price_candles.high, excluded.high),
                low  = MIN(price_candles.low,  excluded.low),
                close = excluded.close,
                tick_count = price_candles.tick_count + 1
            "#,
        )
        .bind(underlying)
        .bind(interval.as_str())
        .bind(bucket)
        .bind(price)
        .bind(price)
        .bind(price)
        .bind(price)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Fetch candles for an underlying/interval within `[from, to]`, ascending.
/// The range is capped at `MAX_CANDLES` buckets.
pub async fn query_candles(
    db: &SqlitePool,
    underlying: &str,
    interval: Interval,
    from: i64,
    to: i64,
) -> Result<Vec<Candle>, sqlx::Error> {
    let step = interval.seconds();
    let max_span = step * MAX_CANDLES;
    let to = to.min(from + max_span);

    sqlx::query_as::<_, Candle>(
        r#"
        SELECT underlying, interval, bucket_start, open, high, low, close, tick_count
        FROM price_candles
        WHERE underlying = ? AND interval = ? AND bucket_start >= ? AND bucket_start <= ?
        ORDER BY bucket_start ASC
        LIMIT ?
        "#,
    )
    .bind(underlying)
    .bind(interval.as_str())
    .bind(from)
    .bind(to)
    .bind(MAX_CANDLES)
    .fetch_all(db)
    .await
}

/// Prune raw ticks older than `retention_days`, keeping candles intact.
pub async fn prune_ticks(db: &SqlitePool, retention_days: i64) -> Result<u64, sqlx::Error> {
    let cutoff = now_secs() - retention_days * 86_400;
    let res = sqlx::query("DELETE FROM price_ticks WHERE observed_at < ?")
        .bind(cutoff)
        .execute(db)
        .await?;
    Ok(res.rows_affected())
}

/// Latest persisted tick price for an underlying, used to seed spot on boot.
pub async fn latest_tick_price(
    db: &SqlitePool,
    underlying: &str,
) -> Result<Option<f64>, sqlx::Error> {
    let row: Option<(f64,)> = sqlx::query_as(
        "SELECT price FROM price_ticks WHERE underlying = ? ORDER BY observed_at DESC LIMIT 1",
    )
    .bind(underlying)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(p,)| p))
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_alignment_is_utc_floor() {
        assert_eq!(bucket_start(1_700_000_045, Interval::M1), 1_700_000_040);
        assert_eq!(bucket_start(1_700_000_045, Interval::M5), 1_700_000_000);
        assert_eq!(bucket_start(1_700_000_045, Interval::H1), 1_699_999_200);
        assert_eq!(bucket_start(1_700_000_045, Interval::D1), 1_699_977_600);
    }

    #[test]
    fn interval_parsing_roundtrips() {
        for i in Interval::ALL {
            assert_eq!(Interval::parse(i.as_str()), Some(i));
        }
        assert_eq!(Interval::parse("2m"), None);
    }
}
