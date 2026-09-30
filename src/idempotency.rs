//! Idempotency support for mutating endpoints.
//!
//! Implements the `Idempotency-Key` header semantics described in the IETF
//! draft "The Idempotency-Key HTTP Header Field". A client may attach an
//! `Idempotency-Key` header to any POST or DELETE request. The first request
//! with a given `(wallet, key)` pair is executed and its response stored; any
//! subsequent request with the same key and the same request body is replayed
//! from storage (with an `Idempotent-Replayed: true` header) instead of being
//! executed a second time.
//!
//! The middleware is intentionally generic so it can be layered over every
//! mutating route without copy-pasting logic into each handler. It runs after
//! authentication has resolved the wallet, since the storage key is scoped to
//! the wallet.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

/// Maximum accepted length of an `Idempotency-Key` header value.
pub const MAX_KEY_LEN: usize = 255;

/// Maximum response body size that will be buffered and stored for replay.
/// Larger responses are streamed through without being persisted.
pub const MAX_RESPONSE_BODY: usize = 64 * 1024;

/// How long a stored idempotency record remains valid.
pub const KEY_TTL_HOURS: i64 = 24;

/// Header used by clients to supply an idempotency key.
pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

/// Header set on replayed responses.
pub const IDEMPOTENT_REPLAYED_HEADER: &str = "idempotent-replayed";

/// State required by the idempotency middleware.
#[derive(Clone)]
pub struct IdempotencyState {
    pub pool: PgPool,
}

/// Outcome of attempting to reserve an idempotency key.
enum Reservation {
    /// The key was newly reserved; the handler should run.
    Acquired,
    /// A completed response exists for this key and body; replay it.
    Replay {
        status_code: i32,
        response_body: Vec<u8>,
    },
    /// The same key was used with a different request body.
    BodyMismatch,
    /// Another request with the same key is currently in flight.
    InProgress,
}

/// Compute the SHA-256 hash of a request body, hex encoded.
fn hash_body(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("{:x}", hasher.finalize())
}

/// Reserve an idempotency key for the given wallet.
///
/// Inserts a `pending` row before the handler runs. If a row already exists:
/// - completed with a matching body hash -> replay the stored response
/// - completed with a different body hash -> 422
/// - still pending -> 409 (request in progress)
/// - expired -> the stale row is replaced and the key is re-acquired
async fn reserve(
    pool: &PgPool,
    wallet: &str,
    key: &str,
    request_hash: &str,
) -> Result<Reservation, sqlx::Error> {
    // Opportunistically clear an expired row so the key can be reused.
    sqlx::query(
        "DELETE FROM idempotency_keys \
         WHERE wallet = $1 AND key = $2 AND created_at < now() - ($3 || ' hours')::interval",
    )
    .bind(wallet)
    .bind(key)
    .bind(KEY_TTL_HOURS.to_string())
    .execute(pool)
    .await?;

    // Try to insert a fresh pending row. `ON CONFLICT DO NOTHING` makes this
    // atomic: exactly one concurrent request wins the insert.
    let inserted = sqlx::query(
        "INSERT INTO idempotency_keys (wallet, key, request_hash, status_code, response_body) \
         VALUES ($1, $2, $3, NULL, NULL) \
         ON CONFLICT (wallet, key) DO NOTHING",
    )
    .bind(wallet)
    .bind(key)
    .bind(request_hash)
    .execute(pool)
    .await?;

    if inserted.rows_affected() == 1 {
        return Ok(Reservation::Acquired);
    }

    // A row already exists; inspect it to decide how to respond.
    let row = sqlx::query(
        "SELECT request_hash, status_code, response_body \
         FROM idempotency_keys WHERE wallet = $1 AND key = $2",
    )
    .bind(wallet)
    .bind(key)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        // The row vanished between insert and select (expired concurrently).
        // Treat as a fresh acquisition attempt.
        return Ok(Reservation::Acquired);
    };

    let stored_hash: String = row.try_get("request_hash")?;
    if stored_hash != request_hash {
        return Ok(Reservation::BodyMismatch);
    }

    let status_code: Option<i32> = row.try_get("status_code")?;
    let response_body: Option<Vec<u8>> = row.try_get("response_body")?;

    match (status_code, response_body) {
        (Some(status_code), Some(response_body)) => Ok(Reservation::Replay {
            status_code,
            response_body,
        }),
        _ => Ok(Reservation::InProgress),
    }
}

/// Persist the final response for a reserved key.
async fn finalize(
    pool: &PgPool,
    wallet: &str,
    key: &str,
    status_code: i32,
    response_body: &[u8],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE idempotency_keys \
         SET status_code = $3, response_body = $4 \
         WHERE wallet = $1 AND key = $2",
    )
    .bind(wallet)
    .bind(key)
    .bind(status_code)
    .bind(response_body)
    .execute(pool)
    .await?;
    Ok(())
}

/// Release a reserved key so the client may retry (used on 5xx / panics).
async fn release(pool: &PgPool, wallet: &str, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM idempotency_keys WHERE wallet = $1 AND key = $2")
        .bind(wallet)
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

/// Extract the wallet identifier resolved by the auth layer.
///
/// The auth middleware stores the authenticated wallet in a request extension.
/// If it is absent the request is not authenticated and idempotency is skipped.
fn wallet_from_request<B>(req: &Request<B>) -> Option<String> {
    req.extensions()
        .get::<crate::auth::AuthenticatedWallet>()
        .map(|w| w.0.clone())
}

/// Tower/axum middleware enforcing idempotency for mutating requests.
pub async fn idempotency_middleware(
    State(state): State<Arc<IdempotencyState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let is_mutation = method == axum::http::Method::POST || method == axum::http::Method::DELETE;

    // Only POST/DELETE are idempotent; GET and others pass through untouched.
    if !is_mutation {
        return next.run(req).await;
    }

    let key = match req.headers().get(IDEMPOTENCY_KEY_HEADER) {
        Some(value) => match value.to_str() {
            Ok(k) if !k.is_empty() && k.len() <= MAX_KEY_LEN => k.to_string(),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    "invalid Idempotency-Key header",
                )
                    .into_response();
            }
        },
        // No key supplied: behave exactly as before.
        None => return next.run(req).await,
    };

    let Some(wallet) = wallet_from_request(&req) else {
        // Unauthenticated requests are handled by the auth layer; skip here.
        return next.run(req).await;
    };

    // Buffer the request body so we can hash it and still forward it.
    let (parts, body) = req.into_parts();
    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "failed to read request body").into_response();
        }
    };
    let request_hash = hash_body(&body_bytes);

    match reserve(&state.pool, &wallet, &key, &request_hash).await {
        Ok(Reservation::Acquired) => {
            // Rebuild the request and run the handler.
            let req = Request::from_parts(parts, Body::from(body_bytes));
            let response = next.run(req).await;
            let status = response.status();

            // Buffer the response body so it can be stored for replay.
            let (resp_parts, resp_body) = response.into_parts();
            let resp_bytes = match resp_body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => {
                    // Could not read the response; release the key so the
                    // client can retry rather than being stuck.
                    let _ = release(&state.pool, &wallet, &key).await;
                    return (StatusCode::INTERNAL_SERVER_ERROR, "response error").into_response();
                }
            };

            if status.is_server_error() {
                // 5xx: release the key so the client may retry.
                let _ = release(&state.pool, &wallet, &key).await;
            } else if resp_bytes.len() <= MAX_RESPONSE_BODY {
                let _ = finalize(
                    &state.pool,
                    &wallet,
                    &key,
                    status.as_u16() as i32,
                    &resp_bytes,
                )
                .await;
            } else {
                // Response too large to store; release so a retry re-executes.
                let _ = release(&state.pool, &wallet, &key).await;
            }

            Response::from_parts(resp_parts, Body::from(resp_bytes))
        }
        Ok(Reservation::Replay {
            status_code,
            response_body,
        }) => {
            let status = StatusCode::from_u16(status_code as u16)
                .unwrap_or(StatusCode::OK);
            let mut response = Response::new(Body::from(response_body));
            *response.status_mut() = status;
            response.headers_mut().insert(
                IDEMPOTENT_REPLAYED_HEADER,
                HeaderValue::from_static("true"),
            );
            response
        }
        Ok(Reservation::BodyMismatch) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Idempotency-Key reused with a different request body",
        )
            .into_response(),
        Ok(Reservation::InProgress) => (
            StatusCode::CONFLICT,
            "request in progress",
        )
            .into_response(),
        Err(_) => {
            // Storage failure: fail closed rather than risk double execution.
            (StatusCode::INTERNAL_SERVER_ERROR, "idempotency store error").into_response()
        }
    }
}

/// Sweep expired idempotency keys. Intended to be driven by the existing
/// cleanup loop alongside other periodic maintenance tasks.
pub async fn cleanup_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM idempotency_keys \
         WHERE created_at < now() - ($1 || ' hours')::interval",
    )
    .bind(KEY_TTL_HOURS.to_string())
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Convenience helper for tests and callers that need the current cutoff.
pub fn expiry_cutoff(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::hours(KEY_TTL_HOURS)
}

/// Build the header map fragment used when replaying a stored response.
pub fn replayed_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        IDEMPOTENT_REPLAYED_HEADER,
        HeaderValue::from_static("true"),
    );
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_distinct() {
        assert_eq!(hash_body(b"abc"), hash_body(b"abc"));
        assert_ne!(hash_body(b"abc"), hash_body(b"abd"));
    }

    #[test]
    fn expiry_cutoff_is_24h() {
        let now = Utc::now();
        assert_eq!(expiry_cutoff(now), now - Duration::hours(24));
    }

    #[test]
    fn replayed_header_is_set() {
        let headers = replayed_headers();
        assert_eq!(headers.get(IDEMPOTENT_REPLAYED_HEADER).unwrap(), "true");
    }
}
