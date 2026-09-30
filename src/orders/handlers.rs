use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::orders::matching::{self, OrderSide, OrderStatus, OrderTif, Quote};
use crate::positions::open_position_in_tx;

/// Shared application state for the orders module.
#[derive(Clone)]
pub struct OrdersState {
    pub pool: PgPool,
    pub quotes: Arc<dyn QuoteSource>,
}

/// Source of the platform's current bid/ask for a series.
#[async_trait::async_trait]
pub trait QuoteSource: Send + Sync {
    async fn quote(&self, series: &str) -> anyhow::Result<Option<Quote>>;
}

/// Router for wallet-scoped, rate-limited order endpoints.
pub fn routes(state: OrdersState) -> Router {
    Router::new()
        .route("/api/v1/orders", post(create_order).get(list_orders))
        .route("/api/v1/orders/:id", get(get_order).delete(cancel_order))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub wallet: String,
    pub series: String,
    pub side: OrderSide,
    pub intent: OrderIntent,
    pub limit_price: Decimal,
    pub contracts: i64,
    pub tif: OrderTif,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderIntent {
    Open,
    Close,
}

#[derive(Debug, Deserialize)]
pub struct ListOrdersQuery {
    pub wallet: String,
    #[serde(default)]
    pub status: Option<OrderStatus>,
}

#[derive(Debug, Serialize)]
pub struct OrderResponse {
    pub id: Uuid,
    pub wallet: String,
    pub series: String,
    pub side: OrderSide,
    pub intent: OrderIntent,
    pub limit_price: Decimal,
    pub contracts: i64,
    pub tif: OrderTif,
    pub status: OrderStatus,
    pub filled_position_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: String,
}

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (status, Json(ApiError { error: msg.into() }))
}

/// `POST /api/v1/orders` — place a resting limit order.
///
/// Balance and margin are reserved up-front; the reservation is released on
/// cancel or expiry. `ioc` orders that cannot fill immediately are rejected.
pub async fn create_order(
    State(state): State<OrdersState>,
    Json(req): Json<CreateOrderRequest>,
) -> impl IntoResponse {
    if req.contracts <= 0 {
        return err(StatusCode::BAD_REQUEST, "contracts must be positive").into_response();
    }
    if req.tif == OrderTif::Gtd && req.expires_at.is_none() {
        return err(StatusCode::BAD_REQUEST, "gtd orders require expires_at").into_response();
    }

    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Reserve balance/margin for the resting order.
    if let Err(e) = reserve_for_order(&mut tx, &req).await {
        return err(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response();
    }

    let row = sqlx::query_as::<_, OrderRow>(
        r#"
        INSERT INTO orders
            (wallet, series, side, intent, limit_price, contracts, tif, status, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, 'open', $8)
        RETURNING id, wallet, series, side, intent, limit_price, contracts, tif,
                  status, filled_position_id, created_at, updated_at
        "#,
    )
    .bind(&req.wallet)
    .bind(&req.series)
    .bind(req.side)
    .bind(req.intent)
    .bind(req.limit_price)
    .bind(req.contracts)
    .bind(req.tif)
    .bind(req.expires_at)
    .fetch_one(&mut *tx)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    if let Err(e) = tx.commit().await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Immediate-or-cancel: attempt a fill now, otherwise reject.
    if req.tif == OrderTif::Ioc {
        match try_fill_now(&state, row.id).await {
            Ok(Some(filled)) => return (StatusCode::CREATED, Json(filled)).into_response(),
            Ok(None) => {
                let _ = reject_and_release(&state, row.id).await;
                return err(StatusCode::CONFLICT, "ioc order could not be filled").into_response();
            }
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    (StatusCode::CREATED, Json(row.into_response())).into_response()
}

/// `GET /api/v1/orders?wallet=&status=` — list wallet-scoped orders.
pub async fn list_orders(
    State(state): State<OrdersState>,
    Query(q): Query<ListOrdersQuery>,
) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, OrderRow>(
        r#"
        SELECT id, wallet, series, side, intent, limit_price, contracts, tif,
               status, filled_position_id, created_at, updated_at
        FROM orders
        WHERE wallet = $1 AND ($2::text IS NULL OR status = $2)
        ORDER BY created_at DESC
        "#,
    )
    .bind(&q.wallet)
    .bind(q.status)
    .fetch_all(&state.pool)
    .await;

    match rows {
        Ok(rows) => {
            let out: Vec<OrderResponse> = rows.into_iter().map(OrderRow::into_response).collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /api/v1/orders/:id` — fetch a single wallet-scoped order.
pub async fn get_order(
    State(state): State<OrdersState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match fetch_order(&state.pool, id).await {
        Ok(Some(row)) => (StatusCode::OK, Json(row.into_response())).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "order not found").into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `DELETE /api/v1/orders/:id` — cancel an open order and release its reservation.
pub async fn cancel_order(
    State(state): State<OrdersState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Guarded transition: only an open order can be cancelled, so a cancel
    // racing a fill loses cleanly.
    let updated = sqlx::query_as::<_, OrderRow>(
        r#"
        UPDATE orders SET status = 'cancelled', updated_at = now()
        WHERE id = $1 AND status = 'open'
        RETURNING id, wallet, series, side, intent, limit_price, contracts, tif,
                  status, filled_position_id, created_at, updated_at
        "#,
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await;

    let row = match updated {
        Ok(Some(row)) => row,
        Ok(None) => return err(StatusCode::CONFLICT, "order is not open").into_response(),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    if let Err(e) = release_reservation(&mut tx, &row).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    if let Err(e) = tx.commit().await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    (StatusCode::OK, Json(row.into_response())).into_response()
}

/// Attempt to fill a single order against the current quote.
async fn try_fill_now(state: &OrdersState, id: Uuid) -> anyhow::Result<Option<OrderResponse>> {
    let Some(row) = fetch_order(&state.pool, id).await? else {
        return Ok(None);
    };
    let Some(quote) = state.quotes.quote(&row.series).await? else {
        return Ok(None);
    };
    let Some(fill) = matching::match_order(&row.to_matching(), &quote) else {
        return Ok(None);
    };
    execute_fill(state, row, fill).await.map(Some)
}

/// Execute a fill transactionally, guarded so a fill can only happen once.
async fn execute_fill(
    state: &OrdersState,
    row: OrderRow,
    fill: matching::Fill,
) -> anyhow::Result<OrderResponse> {
    let mut tx = state.pool.begin().await?;

    let claimed = sqlx::query_scalar::<_, Uuid>(
        "UPDATE orders SET status = 'filled', updated_at = now() WHERE id = $1 AND status = 'open' RETURNING id",
    )
    .bind(row.id)
    .fetch_optional(&mut *tx)
    .await?;

    if claimed.is_none() {
        tx.rollback().await?;
        anyhow::bail!("order already filled or cancelled");
    }

    // Reuse the existing open/close transaction helper for execution.
    let position_id = open_position_in_tx(
        &mut tx,
        &row.wallet,
        &row.series,
        row.side,
        fill.price,
        row.contracts,
    )
    .await?;

    let updated = sqlx::query_as::<_, OrderRow>(
        r#"
        UPDATE orders SET filled_position_id = $2, updated_at = now()
        WHERE id = $1
        RETURNING id, wallet, series, side, intent, limit_price, contracts, tif,
                  status, filled_position_id, created_at, updated_at
        "#,
    )
    .bind(row.id)
    .bind(position_id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(updated.into_response())
}

/// Reject an order and release its reservation (used for unfillable IOC).
async fn reject_and_release(state: &OrdersState, id: Uuid) -> anyhow::Result<()> {
    let mut tx = state.pool.begin().await?;
    let row = sqlx::query_as::<_, OrderRow>(
        r#"
        UPDATE orders SET status = 'rejected', updated_at = now()
        WHERE id = $1 AND status = 'open'
        RETURNING id, wallet, series, side, intent, limit_price, contracts, tif,
                  status, filled_position_id, created_at, updated_at
        "#,
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = row {
        release_reservation(&mut tx, &row).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn fetch_order(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<OrderRow>> {
    let row = sqlx::query_as::<_, OrderRow>(
        r#"
        SELECT id, wallet, series, side, intent, limit_price, contracts, tif,
               status, filled_position_id, created_at, updated_at
        FROM orders WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Reserve balance and margin for a resting order.
async fn reserve_for_order(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    req: &CreateOrderRequest,
) -> anyhow::Result<()> {
    let notional = req.limit_price * Decimal::from(req.contracts);
    let affected = sqlx::query(
        r#"
        UPDATE wallets
        SET reserved_balance = reserved_balance + $2,
            reserved_margin = reserved_margin + $3
        WHERE address = $1 AND balance - reserved_balance >= $2
        "#,
    )
    .bind(&req.wallet)
    .bind(notional)
    .bind(notional)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    if affected == 0 {
        anyhow::bail!("insufficient available balance to reserve for order");
    }
    Ok(())
}

/// Release a reservation when an order is cancelled, expired, or rejected.
async fn release_reservation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    row: &OrderRow,
) -> anyhow::Result<()> {
    let notional = row.limit_price * Decimal::from(row.contracts);
    sqlx::query(
        r#"
        UPDATE wallets
        SET reserved_balance = GREATEST(reserved_balance - $2, 0),
            reserved_margin = GREATEST(reserved_margin - $3, 0)
        WHERE address = $1
        "#,
    )
    .bind(&row.wallet)
    .bind(notional)
    .bind(notional)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
pub struct OrderRow {
    pub id: Uuid,
    pub wallet: String,
    pub series: String,
    pub side: OrderSide,
    pub intent: OrderIntent,
    pub limit_price: Decimal,
    pub contracts: i64,
    pub tif: OrderTif,
    pub status: OrderStatus,
    pub filled_position_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl OrderRow {
    fn to_matching(&self) -> matching::RestingOrder {
        matching::RestingOrder {
            id: self.id,
            side: self.side,
            limit_price: self.limit_price,
            contracts: self.contracts,
        }
    }

    fn into_response(self) -> OrderResponse {
        OrderResponse {
            id: self.id,
            wallet: self.wallet,
            series: self.series,
            side: self.side,
            intent: self.intent,
            limit_price: self.limit_price,
            contracts: self.contracts,
            tif: self.tif,
            status: self.status,
            filled_position_id: self.filled_position_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
