//! Paper account lifecycle: faucet funding, atomic reset, and epoch-scoped history.
//!
//! This module implements the account lifecycle endpoints described in issue #35:
//! * `POST /api/v1/account/faucet` — rate-limited, capped funding.
//! * `POST /api/v1/account/reset` — atomic reset that closes open positions at
//!   mark with reason `reset`, cancels resting orders, restores the default
//!   balance, and starts a new epoch while archiving the prior session.
//!
//! Every funding action is recorded in the `account_audit` table so balances can
//! be reconstructed and audited.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::positions::{close_position_in_tx, Position};

/// Default starting balance for a fresh paper account.
pub const DEFAULT_BALANCE: i64 = 100_000;

/// Maximum number of faucet grants allowed per wallet per day.
pub const FAUCET_DAILY_LIMIT: i64 = 3;

/// Amount credited by a single faucet grant.
pub const FAUCET_AMOUNT: i64 = 10_000;

/// Configurable ceiling on total account balance.
pub const FAUCET_BALANCE_CEILING: i64 = 1_000_000;

#[derive(Clone)]
pub struct AccountLifecycleState {
    pub pool: PgPool,
    pub default_balance: i64,
    pub faucet_daily_limit: i64,
    pub faucet_amount: i64,
    pub faucet_balance_ceiling: i64,
}

impl AccountLifecycleState {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            default_balance: DEFAULT_BALANCE,
            faucet_daily_limit: FAUCET_DAILY_LIMIT,
            faucet_amount: FAUCET_AMOUNT,
            faucet_balance_ceiling: FAUCET_BALANCE_CEILING,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AccountResponse {
    pub wallet: String,
    pub balance: i64,
    pub epoch_id: Uuid,
    pub epoch_started_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct FaucetResponse {
    pub wallet: String,
    pub credited: i64,
    pub balance: i64,
    pub grants_today: i64,
    pub daily_limit: i64,
}

#[derive(Debug, Serialize)]
pub struct ResetResponse {
    pub wallet: String,
    pub balance: i64,
    pub closed_positions: i64,
    pub cancelled_orders: i64,
    pub archived_epoch_id: Uuid,
    pub new_epoch_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct EpochQuery {
    /// `current` (default) scopes history/stats/leaderboards to the active epoch;
    /// `all` includes every archived epoch.
    #[serde(default)]
    pub epoch: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("account not found")]
    NotFound,
    #[error("faucet daily limit reached")]
    FaucetLimitReached,
    #[error("balance ceiling reached")]
    BalanceCeilingReached,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for AccountError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            AccountError::NotFound => StatusCode::NOT_FOUND,
            AccountError::FaucetLimitReached | AccountError::BalanceCeilingReached => {
                StatusCode::TOO_MANY_REQUESTS
            }
            AccountError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}

pub fn router(state: Arc<AccountLifecycleState>) -> Router {
    Router::new()
        .route("/api/v1/account/faucet", post(faucet))
        .route("/api/v1/account/reset", post(reset))
        .with_state(state)
}

/// Resolve the epoch id that scopes history/stats/leaderboards.
///
/// `epoch=all` returns `None`, signalling callers to include archived epochs.
pub async fn resolve_epoch_scope(
    pool: &PgPool,
    wallet: &str,
    query: &EpochQuery,
) -> Result<Option<Uuid>, sqlx::Error> {
    if query.epoch.as_deref() == Some("all") {
        return Ok(None);
    }
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM account_epochs WHERE wallet = $1 AND closed_at IS NULL \
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(wallet)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Ensure an account row and an open epoch exist, creating them lazily.
async fn ensure_account(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &str,
    default_balance: i64,
) -> Result<(i64, Uuid), sqlx::Error> {
    // Serialise on the account row to guard against in-flight fills.
    sqlx::query(
        "INSERT INTO accounts (wallet, balance) VALUES ($1, $2) \
         ON CONFLICT (wallet) DO NOTHING",
    )
    .bind(wallet)
    .bind(default_balance)
    .execute(&mut **tx)
    .await?;

    let (balance,): (i64,) =
        sqlx::query_as("SELECT balance FROM accounts WHERE wallet = $1 FOR UPDATE")
            .bind(wallet)
            .fetch_one(&mut **tx)
            .await?;

    let epoch: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM account_epochs WHERE wallet = $1 AND closed_at IS NULL \
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(wallet)
    .fetch_optional(&mut **tx)
    .await?;

    let epoch_id = match epoch {
        Some((id,)) => id,
        None => {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO account_epochs (wallet, starting_balance) VALUES ($1, $2) \
                 RETURNING id",
            )
            .bind(wallet)
            .bind(balance)
            .fetch_one(&mut **tx)
            .await?;
            id
        }
    };

    Ok((balance, epoch_id))
}

async fn record_audit(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &str,
    epoch_id: Uuid,
    kind: &str,
    amount: i64,
    balance_after: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO account_audit (wallet, epoch_id, kind, amount, balance_after) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(wallet)
    .bind(epoch_id)
    .bind(kind)
    .bind(amount)
    .bind(balance_after)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// `POST /api/v1/account/faucet`
///
/// Credits a fixed amount, enforcing a per-wallet daily grant limit and a
/// configurable ceiling on total balance. Every grant is written to the audit
/// table.
pub async fn faucet(
    State(state): State<Arc<AccountLifecycleState>>,
    Json(req): Json<WalletRequest>,
) -> Result<Json<FaucetResponse>, AccountError> {
    let mut tx = state.pool.begin().await?;
    let (balance, epoch_id) =
        ensure_account(&mut tx, &req.wallet, state.default_balance).await?;

    let since = Utc::now() - Duration::days(1);
    let (grants_today,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM account_audit \
         WHERE wallet = $1 AND kind = 'faucet' AND created_at >= $2",
    )
    .bind(&req.wallet)
    .bind(since)
    .fetch_one(&mut *tx)
    .await?;

    if grants_today >= state.faucet_daily_limit {
        return Err(AccountError::FaucetLimitReached);
    }

    let credited = state.faucet_amount.min(state.faucet_balance_ceiling - balance);
    if credited <= 0 {
        return Err(AccountError::BalanceCeilingReached);
    }

    let new_balance = balance + credited;
    sqlx::query("UPDATE accounts SET balance = $1 WHERE wallet = $2")
        .bind(new_balance)
        .bind(&req.wallet)
        .execute(&mut *tx)
        .await?;

    record_audit(&mut tx, &req.wallet, epoch_id, "faucet", credited, new_balance).await?;
    tx.commit().await?;

    Ok(Json(FaucetResponse {
        wallet: req.wallet,
        credited,
        balance: new_balance,
        grants_today: grants_today + 1,
        daily_limit: state.faucet_daily_limit,
    }))
}

/// `POST /api/v1/account/reset`
///
/// Atomically closes every open position at mark with reason `reset`, cancels
/// resting orders, restores the default balance, archives the prior epoch, and
/// starts a new one. Serialises on the account row so an in-flight fill cannot
/// interleave.
pub async fn reset(
    State(state): State<Arc<AccountLifecycleState>>,
    Json(req): Json<WalletRequest>,
) -> Result<Json<ResetResponse>, AccountError> {
    let mut tx = state.pool.begin().await?;
    let (_balance, epoch_id) =
        ensure_account(&mut tx, &req.wallet, state.default_balance).await?;

    // Close every open position at mark, tagged with reason `reset`.
    let open: Vec<Position> = sqlx::query_as(
        "SELECT * FROM positions WHERE wallet = $1 AND epoch_id = $2 AND closed_at IS NULL \
         FOR UPDATE",
    )
    .bind(&req.wallet)
    .bind(epoch_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut closed_positions = 0i64;
    for position in &open {
        close_position_in_tx(&mut tx, position, "reset").await?;
        closed_positions += 1;
    }

    // Cancel any resting orders so they cannot fill into the new epoch.
    let cancelled = sqlx::query(
        "UPDATE orders SET status = 'cancelled', cancelled_at = now() \
         WHERE wallet = $1 AND status IN ('open', 'resting', 'pending')",
    )
    .bind(&req.wallet)
    .execute(&mut *tx)
    .await?
    .rows_affected() as i64;

    // Archive the prior epoch and start a fresh one.
    sqlx::query("UPDATE account_epochs SET closed_at = now() WHERE id = $1")
        .bind(epoch_id)
        .execute(&mut *tx)
        .await?;

    let (new_epoch_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO account_epochs (wallet, starting_balance) VALUES ($1, $2) RETURNING id",
    )
    .bind(&req.wallet)
    .bind(state.default_balance)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query("UPDATE accounts SET balance = $1 WHERE wallet = $2")
        .bind(state.default_balance)
        .bind(&req.wallet)
        .execute(&mut *tx)
        .await?;

    record_audit(
        &mut tx,
        &req.wallet,
        new_epoch_id,
        "reset",
        state.default_balance,
        state.default_balance,
    )
    .await?;

    tx.commit().await?;

    Ok(Json(ResetResponse {
        wallet: req.wallet,
        balance: state.default_balance,
        closed_positions,
        cancelled_orders: cancelled,
        archived_epoch_id: epoch_id,
        new_epoch_id,
    }))
}

#[derive(Debug, Deserialize)]
pub struct WalletRequest {
    pub wallet: String,
}

/// Convenience helper for callers that only need the current epoch id.
pub async fn current_epoch(pool: &PgPool, wallet: &str) -> Result<Option<Uuid>, sqlx::Error> {
    resolve_epoch_scope(pool, wallet, &EpochQuery { epoch: None }).await
}

/// Query helper: scope a history/leaderboard query to an epoch when provided.
pub fn epoch_filter(epoch_id: Option<Uuid>) -> (&'static str, Option<Uuid>) {
    match epoch_id {
        Some(id) => ("AND epoch_id = $2", Some(id)),
        None => ("", None),
    }
}

#[allow(dead_code)]
fn _assert_query_param_used(_: &Query<EpochQuery>) {}
