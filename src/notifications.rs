//! Exercise, assignment and expiry notification pipeline (issue #43).
//!
//! Provides the notification inbox: a `notifications` table with a unique
//! `(wallet, kind, subject_id)` dedupe key, cursor-paginated listing, read
//! marking, per-wallet preferences, and helpers used by the expiry scheduler
//! and the settlement engine to emit lifecycle notices through the outbox.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::ApiError;
use crate::outbox::OutboxEvent;

/// Lifecycle notification kinds emitted by the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// Expiry is approaching (T-24h).
    ExpiryApproaching24h,
    /// Expiry is approaching (T-1h).
    ExpiryApproaching1h,
    /// Option expired in the money; carries the settlement amount.
    ExpiredItm,
    /// Option expired out of the money.
    ExpiredOtm,
    /// Short position was assigned.
    Assigned,
    /// Collateral was released back to the wallet.
    CollateralReleased,
}

impl NotificationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NotificationKind::ExpiryApproaching24h => "expiry_approaching_24h",
            NotificationKind::ExpiryApproaching1h => "expiry_approaching_1h",
            NotificationKind::ExpiredItm => "expired_itm",
            NotificationKind::ExpiredOtm => "expired_otm",
            NotificationKind::Assigned => "assigned",
            NotificationKind::CollateralReleased => "collateral_released",
        }
    }
}

/// A row in the notification inbox.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub wallet: String,
    pub kind: String,
    pub subject_id: Uuid,
    pub payload: serde_json::Value,
    pub read_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

/// Cursor-paginated notification list response.
#[derive(Debug, Serialize)]
pub struct NotificationPage {
    pub items: Vec<Notification>,
    pub next_cursor: Option<String>,
}

/// Per-wallet notification preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPreferences {
    pub expiry_reminders: bool,
    pub settlement_outcomes: bool,
    pub assignments: bool,
    pub collateral_released: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            expiry_reminders: true,
            settlement_outcomes: true,
            assignments: true,
            collateral_released: true,
        }
    }
}

impl NotificationPreferences {
    /// Whether a given kind should be delivered for this wallet.
    pub fn allows(&self, kind: NotificationKind) -> bool {
        match kind {
            NotificationKind::ExpiryApproaching24h | NotificationKind::ExpiryApproaching1h => {
                self.expiry_reminders
            }
            NotificationKind::ExpiredItm | NotificationKind::ExpiredOtm => {
                self.settlement_outcomes
            }
            NotificationKind::Assigned => self.assignments,
            NotificationKind::CollateralReleased => self.collateral_released,
        }
    }
}

/// A position that is approaching expiry, used by the reminder scheduler.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExpiringPosition {
    pub id: Uuid,
    pub wallet: String,
    pub symbol: String,
    pub expires_at: OffsetDateTime,
}

/// Insert a notification, deduplicating on `(wallet, kind, subject_id)`.
///
/// Returns `true` when a new row was written and `false` when the notice was
/// already emitted (the unique constraint suppressed the duplicate).
#[allow(clippy::too_many_arguments)]
pub async fn emit(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &str,
    kind: NotificationKind,
    subject_id: Uuid,
    payload: serde_json::Value,
) -> Result<bool, ApiError> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        INSERT INTO notifications (id, wallet, kind, subject_id, payload, created_at)
        VALUES ($1, $2, $3, $4, $5, now())
        ON CONFLICT (wallet, kind, subject_id) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(wallet)
    .bind(kind.as_str())
    .bind(subject_id)
    .bind(payload)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(row.is_some())
}

/// Emit a notification and mirror it onto the event bus via the outbox, in the
/// caller's transaction. Used by settlement so outcome notices commit atomically.
#[allow(clippy::too_many_arguments)]
pub async fn emit_with_outbox(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &str,
    kind: NotificationKind,
    subject_id: Uuid,
    payload: serde_json::Value,
) -> Result<bool, ApiError> {
    let inserted = emit(tx, wallet, kind, subject_id, payload.clone()).await?;
    if inserted {
        let event = OutboxEvent::new(
            "notification.created",
            serde_json::json!({
                "wallet": wallet,
                "kind": kind.as_str(),
                "subject_id": subject_id,
                "payload": payload,
            }),
        );
        event.persist(tx).await?;
    }
    Ok(inserted)
}

/// Emit settlement outcome notices for a settled position in the settlement
/// transaction. Covers expired ITM/OTM (with settlement amount), assignment for
/// short ITM positions, and collateral released.
#[allow(clippy::too_many_arguments)]
pub async fn emit_settlement_outcomes(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &str,
    position_id: Uuid,
    symbol: &str,
    is_short: bool,
    in_the_money: bool,
    settlement_amount: i64,
    collateral_released: i64,
) -> Result<(), ApiError> {
    let prefs = load_preferences_tx(tx, wallet).await?;

    let outcome_kind = if in_the_money {
        NotificationKind::ExpiredItm
    } else {
        NotificationKind::ExpiredOtm
    };
    if prefs.allows(outcome_kind) {
        emit_with_outbox(
            tx,
            wallet,
            outcome_kind,
            position_id,
            serde_json::json!({
                "symbol": symbol,
                "settlement_amount": settlement_amount,
            }),
        )
        .await?;
    }

    if is_short && in_the_money && prefs.allows(NotificationKind::Assigned) {
        emit_with_outbox(
            tx,
            wallet,
            NotificationKind::Assigned,
            position_id,
            serde_json::json!({
                "symbol": symbol,
                "settlement_amount": settlement_amount,
            }),
        )
        .await?;
    }

    if collateral_released > 0 && prefs.allows(NotificationKind::CollateralReleased) {
        emit_with_outbox(
            tx,
            wallet,
            NotificationKind::CollateralReleased,
            position_id,
            serde_json::json!({
                "symbol": symbol,
                "amount": collateral_released,
            }),
        )
        .await?;
    }

    Ok(())
}

/// Run the expiry reminder scheduler for the given clock instant.
///
/// Emits T-24h and T-1h notices exactly once per position. Positions that have
/// already expired or been closed are skipped. When the server was down, stale
/// reminders are collapsed: only the most recent applicable window fires.
pub async fn run_expiry_reminders(
    pool: &PgPool,
    now: OffsetDateTime,
) -> Result<usize, ApiError> {
    let horizon = now + time::Duration::hours(24);
    let positions: Vec<ExpiringPosition> = sqlx::query_as(
        r#"
        SELECT id, wallet, symbol, expires_at
        FROM positions
        WHERE status = 'open'
          AND expires_at > $1
          AND expires_at <= $2
        "#,
    )
    .bind(now)
    .bind(horizon)
    .fetch_all(pool)
    .await?;

    let mut emitted = 0usize;
    for position in positions {
        let remaining = position.expires_at - now;
        // Collapse stale reminders: pick the tightest window that still applies.
        let kind = if remaining <= time::Duration::hours(1) {
            NotificationKind::ExpiryApproaching1h
        } else {
            NotificationKind::ExpiryApproaching24h
        };

        let mut tx = pool.begin().await?;
        let prefs = load_preferences_tx(&mut tx, &position.wallet).await?;
        if prefs.allows(kind) {
            let payload = serde_json::json!({
                "symbol": position.symbol,
                "expires_at": position.expires_at,
            });
            if emit_with_outbox(&mut tx, &position.wallet, kind, position.id, payload).await? {
                emitted += 1;
            }
        }
        tx.commit().await?;
    }

    Ok(emitted)
}

/// List notifications for a wallet, cursor-paginated by `(created_at, id)`.
pub async fn list(
    pool: &PgPool,
    wallet: &str,
    cursor: Option<(OffsetDateTime, Uuid)>,
    limit: i64,
) -> Result<NotificationPage, ApiError> {
    let limit = limit.clamp(1, 100);
    let (cursor_ts, cursor_id) = match cursor {
        Some((ts, id)) => (Some(ts), Some(id)),
        None => (None, None),
    };

    let mut items: Vec<Notification> = sqlx::query_as(
        r#"
        SELECT id, wallet, kind, subject_id, payload, read_at, created_at
        FROM notifications
        WHERE wallet = $1
          AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4
        "#,
    )
    .bind(wallet)
    .bind(cursor_ts)
    .bind(cursor_id)
    .bind(limit + 1)
    .fetch_all(pool)
    .await?;

    let next_cursor = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        items.last().map(|n| encode_cursor(n.created_at, n.id))
    } else {
        None
    };

    Ok(NotificationPage { items, next_cursor })
}

/// Mark a single notification as read. Returns `false` if not found for wallet.
pub async fn mark_read(pool: &PgPool, wallet: &str, id: Uuid) -> Result<bool, ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE notifications
        SET read_at = COALESCE(read_at, now())
        WHERE id = $1 AND wallet = $2
        "#,
    )
    .bind(id)
    .bind(wallet)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Mark all unread notifications for a wallet as read. Returns the count.
pub async fn mark_all_read(pool: &PgPool, wallet: &str) -> Result<u64, ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE notifications
        SET read_at = now()
        WHERE wallet = $1 AND read_at IS NULL
        "#,
    )
    .bind(wallet)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Load per-wallet notification preferences, falling back to defaults.
pub async fn load_preferences(pool: &PgPool, wallet: &str) -> Result<NotificationPreferences, ApiError> {
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT preferences FROM notification_preferences WHERE wallet = $1",
    )
    .bind(wallet)
    .fetch_optional(pool)
    .await?;

    Ok(row
        .and_then(|(v,)| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

async fn load_preferences_tx(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &str,
) -> Result<NotificationPreferences, ApiError> {
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT preferences FROM notification_preferences WHERE wallet = $1",
    )
    .bind(wallet)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(row
        .and_then(|(v,)| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

/// Persist per-wallet notification preferences.
pub async fn save_preferences(
    pool: &PgPool,
    wallet: &str,
    prefs: &NotificationPreferences,
) -> Result<(), ApiError> {
    let value = serde_json::to_value(prefs)?;
    sqlx::query(
        r#"
        INSERT INTO notification_preferences (wallet, preferences, updated_at)
        VALUES ($1, $2, now())
        ON CONFLICT (wallet) DO UPDATE
        SET preferences = EXCLUDED.preferences, updated_at = now()
        "#,
    )
    .bind(wallet)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

fn encode_cursor(created_at: OffsetDateTime, id: Uuid) -> String {
    format!("{}|{}", created_at.unix_timestamp_nanos(), id)
}

/// Decode a cursor produced by [`encode_cursor`].
pub fn decode_cursor(cursor: &str) -> Option<(OffsetDateTime, Uuid)> {
    let (ts, id) = cursor.split_once('|')?;
    let nanos: i128 = ts.parse().ok()?;
    let created_at = OffsetDateTime::from_unix_timestamp_nanos(nanos).ok()?;
    let id = Uuid::parse_str(id).ok()?;
    Some((created_at, id))
}

/// Filter a set of kinds down to those enabled for the wallet.
pub fn filter_allowed(
    prefs: &NotificationPreferences,
    kinds: &[NotificationKind],
) -> HashSet<&'static str> {
    kinds
        .iter()
        .filter(|k| prefs.allows(**k))
        .map(|k| k.as_str())
        .collect()
}
