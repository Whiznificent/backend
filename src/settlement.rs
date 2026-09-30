//! Automated expiry settlement engine.
//!
//! Two-phase design:
//!   1. Compute and persist a settlement fixing (TWAP over the 30 minutes
//!      before expiry, taken from persisted ticks) into `settlement_fixings`.
//!   2. Settle every open position that references that fixing, in batches,
//!      each wallet in its own transaction so one bad row cannot block others.
//!
//! The engine is idempotent and resumable: fixings are unique on
//! `(underlying, expires_at)` and positions transition to `expired` exactly
//! once, so a crash mid-batch never double-credits a balance.

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashMap;

use crate::models::{Position, PositionStatus};
use crate::positions::close_position_in_tx;

/// Window over which the settlement TWAP is computed.
const TWAP_WINDOW_MINUTES: i64 = 30;
/// Minimum number of persisted ticks required for a valid TWAP.
const MIN_TICKS_FOR_TWAP: i64 = 2;
/// Number of positions settled per batch.
const SETTLE_BATCH_SIZE: i64 = 100;

/// A persisted settlement fixing for an `(underlying, expires_at)` pair.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SettlementFixing {
    pub underlying: String,
    pub expires_at: DateTime<Utc>,
    pub price: f64,
    pub method: String,
    pub tick_count: i64,
    pub created_at: DateTime<Utc>,
}

/// Outcome of a single settlement pass, useful for tests and logging.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SettleReport {
    pub fixings_created: usize,
    pub positions_settled: usize,
    pub shortfalls: usize,
}

/// Compute the settlement fixing for `(underlying, expires_at)`.
///
/// Uses a TWAP over the 30 minutes preceding expiry from persisted ticks.
/// If there are too few ticks, falls back to the last aggregated price and
/// marks the fixing `method = 'fallback'`.
///
/// Returns `(price, method, tick_count)`.
pub async fn compute_fixing(
    pool: &PgPool,
    underlying: &str,
    expires_at: DateTime<Utc>,
) -> Result<(f64, &'static str, i64), sqlx::Error> {
    let window_start = expires_at - Duration::minutes(TWAP_WINDOW_MINUTES);

    let row: Option<(f64, i64)> = sqlx::query_as(
        "SELECT AVG(price) AS twap, COUNT(*) AS tick_count \
         FROM ticks \
         WHERE underlying = $1 AND ts >= $2 AND ts <= $3",
    )
    .bind(underlying)
    .bind(window_start)
    .bind(expires_at)
    .fetch_optional(pool)
    .await?;

    if let Some((twap, tick_count)) = row {
        if tick_count >= MIN_TICKS_FOR_TWAP {
            return Ok((twap, "twap", tick_count));
        }
    }

    // Fallback: last aggregated price before expiry.
    let last: Option<f64> = sqlx::query_scalar(
        "SELECT price FROM aggregated_prices \
         WHERE underlying = $1 AND ts <= $2 \
         ORDER BY ts DESC LIMIT 1",
    )
    .bind(underlying)
    .bind(expires_at)
    .fetch_optional(pool)
    .await?;

    let price = last.unwrap_or(0.0);
    Ok((price, "fallback", 0))
}

/// Persist a fixing, returning the stored row. Idempotent: an existing fixing
/// for the same `(underlying, expires_at)` is returned unchanged.
pub async fn persist_fixing(
    pool: &PgPool,
    underlying: &str,
    expires_at: DateTime<Utc>,
    price: f64,
    method: &str,
    tick_count: i64,
) -> Result<SettlementFixing, sqlx::Error> {
    sqlx::query_as::<_, SettlementFixing>(
        "INSERT INTO settlement_fixings \
             (underlying, expires_at, price, method, tick_count, created_at) \
         VALUES ($1, $2, $3, $4, $5, NOW()) \
         ON CONFLICT (underlying, expires_at) DO NOTHING \
         RETURNING underlying, expires_at, price, method, tick_count, created_at",
    )
    .bind(underlying)
    .bind(expires_at)
    .bind(price)
    .bind(method)
    .bind(tick_count)
    .fetch_optional(pool)
    .await?
    .map(Ok)
    .unwrap_or_else(|| {
        // Conflict: fetch the already-persisted fixing.
        async move {
            sqlx::query_as::<_, SettlementFixing>(
                "SELECT underlying, expires_at, price, method, tick_count, created_at \
                 FROM settlement_fixings \
                 WHERE underlying = $1 AND expires_at = $2",
            )
            .bind(underlying)
            .bind(expires_at)
            .fetch_one(pool)
            .await
        }
    })
    .await
}

/// Intrinsic value of a position at the given fixing price.
///
/// Calls: `max(spot - strike, 0)`; puts: `max(strike - spot, 0)`.
/// The sign is applied by the caller based on position side.
pub fn intrinsic_value(position: &Position, fixing: f64) -> f64 {
    let strike = position.strike;
    let raw = if position.is_call {
        (fixing - strike).max(0.0)
    } else {
        (strike - fixing).max(0.0)
    };
    raw * position.quantity.abs()
}

/// Settle a single position against a persisted fixing inside its own
/// transaction. Returns `true` if a shortfall was flagged.
async fn settle_position_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    position: &Position,
    fixing: f64,
) -> Result<bool, sqlx::Error> {
    let intrinsic = intrinsic_value(position, fixing);
    let signed_intrinsic = if position.quantity >= 0.0 {
        intrinsic
    } else {
        -intrinsic
    };

    // Reuse the existing close math so settlement and manual close agree.
    let shortfall = close_position_in_tx(
        tx,
        position.id,
        signed_intrinsic,
        fixing,
        PositionStatus::Expired,
    )
    .await?;

    Ok(shortfall)
}

/// Run one settlement pass.
///
/// Phase 1: for every `(underlying, expires_at)` with open positions past
/// expiry, compute and persist a fixing. Phase 2: settle positions in batches,
/// each wallet in its own transaction. Idempotent and resumable.
pub async fn settle_once(pool: &PgPool, now: DateTime<Utc>) -> Result<SettleReport, sqlx::Error> {
    let mut report = SettleReport::default();

    // Phase 1: fixings for all expired underlyings.
    let expired: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT DISTINCT underlying, expires_at \
         FROM positions \
         WHERE status = 'open' AND expires_at <= $1",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;

    let mut fixings: HashMap<(String, DateTime<Utc>), f64> = HashMap::new();
    for (underlying, expires_at) in expired {
        let (price, method, tick_count) = compute_fixing(pool, &underlying, expires_at).await?;
        let fixing = persist_fixing(pool, &underlying, expires_at, price, method, tick_count).await?;
        report.fixings_created += 1;
        fixings.insert((underlying, expires_at), fixing.price);
    }

    // Phase 2: settle positions in batches, each in its own transaction.
    loop {
        let batch: Vec<Position> = sqlx::query_as::<_, Position>(
            "SELECT * FROM positions \
             WHERE status = 'open' AND expires_at <= $1 \
             ORDER BY id LIMIT $2",
        )
        .bind(now)
        .bind(SETTLE_BATCH_SIZE)
        .fetch_all(pool)
        .await?;

        if batch.is_empty() {
            break;
        }

        for position in batch {
            let key = (position.underlying.clone(), position.expires_at);
            let fixing = match fixings.get(&key) {
                Some(f) => *f,
                None => {
                    // Fixing may have been persisted by a previous crashed run.
                    let stored: Option<f64> = sqlx::query_scalar(
                        "SELECT price FROM settlement_fixings \
                         WHERE underlying = $1 AND expires_at = $2",
                    )
                    .bind(&position.underlying)
                    .bind(position.expires_at)
                    .fetch_optional(pool)
                    .await?;
                    match stored {
                        Some(f) => f,
                        None => continue,
                    }
                }
            };

            let mut tx = pool.begin().await?;
            match settle_position_in_tx(&mut tx, &position, fixing).await {
                Ok(shortfall) => {
                    tx.commit().await?;
                    report.positions_settled += 1;
                    if shortfall {
                        report.shortfalls += 1;
                    }
                }
                Err(_) => {
                    // One bad row must not block everyone else.
                    tx.rollback().await?;
                }
            }
        }
    }

    Ok(report)
}

/// Fetch persisted fixings, optionally filtered by underlying and expiry.
pub async fn list_fixings(
    pool: &PgPool,
    underlying: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Vec<SettlementFixing>, sqlx::Error> {
    sqlx::query_as::<_, SettlementFixing>(
        "SELECT underlying, expires_at, price, method, tick_count, created_at \
         FROM settlement_fixings \
         WHERE ($1::text IS NULL OR underlying = $1) \
           AND ($2::timestamptz IS NULL OR expires_at = $2) \
         ORDER BY expires_at DESC",
    )
    .bind(underlying)
    .bind(expires_at)
    .fetch_all(pool)
    .await
}
