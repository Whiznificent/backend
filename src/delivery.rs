//! Verified user notifications and signed webhook delivery.
//!
//! The router is intentionally assembled by the application crate: this module
//! exports handlers but does not modify the central router. Event producers
//! should call [`emit_event`] with a wallet identity they obtained internally
//! (for example from the alert or position row), never from request JSON.
//!
//! Webhooks receive `X-Zenith-Timestamp` (Unix seconds) and
//! `X-Zenith-Signature: sha256=<hex>`. The signature is HMAC-SHA256 over the
//! UTF-8 bytes of `<timestamp>.<raw HTTP request body>`.

use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, StatusCode},
    response::Json,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::{net::IpAddr, time::Duration};

use crate::{
    auth::AuthUser,
    error::{db_error, AppError, AppJson},
    AppState,
};

const EVENT_TYPES: [&str; 4] = [
    "alert_triggered",
    "position_settled",
    "position_liquidated",
    "order_filled",
];
const NOTIFICATION_EVENT_TYPES: [&str; 6] = [
    "alert_triggered",
    "position_settled",
    "position_liquidated",
    "order_filled",
    "margin_call",
    "liquidation",
];
const MAX_PAYLOAD_BYTES: usize = 32 * 1024;
const MAX_EVENT_PAYLOAD_BYTES: usize = 30 * 1024;
const MAX_HTTP_SECONDS: &str = "8";
const VERIFY_TTL_SECS: i64 = 15 * 60;

fn retry_delay(attempts: i64) -> i64 {
    60i64
        .saturating_mul(1i64 << attempts.saturating_sub(1).clamp(0, 11))
        .min(86_400)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn timestamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

fn random_hex(bytes: usize) -> String {
    let mut value = vec![0; bytes];
    rand::thread_rng().fill_bytes(&mut value);
    hex_encode(&value)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 15) as usize] as char);
    }
    out
}

/// Small standalone SHA-256 implementation, keeping this module usable with
/// the repository's current dependency manifest.
fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut data = input.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());

    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for chunk in data.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, bytes) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (dst, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *dst = dst.wrapping_add(value);
        }
    }
    let mut out = [0; 32];
    for (chunk, value) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    out
}

/// Returns the lowercase hex HMAC-SHA256 signature for `timestamp.payload`.
pub fn sign_payload(secret: &[u8], timestamp: i64, payload: &[u8]) -> String {
    let mut message = timestamp.to_string().into_bytes();
    message.push(b'.');
    message.extend_from_slice(payload);
    let mut key = [0; 64];
    if secret.len() > 64 {
        key[..32].copy_from_slice(&sha256(secret));
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut inner = Vec::with_capacity(64 + message.len());
    inner.extend(key.iter().map(|b| *b ^ 0x36));
    inner.extend_from_slice(&message);
    let inner_hash = sha256(&inner);
    let mut outer = Vec::with_capacity(96);
    outer.extend(key.iter().map(|b| *b ^ 0x5c));
    outer.extend_from_slice(&inner_hash);
    hex_encode(&sha256(&outer))
}

fn allowed_event(event: &str) -> bool {
    EVENT_TYPES.contains(&event)
}

#[derive(Debug, Serialize, FromRow)]
pub struct WebhookEndpoint {
    pub id: String,
    pub url: String,
    pub event_types: String,
    pub active: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize, FromRow)]
pub struct Channel {
    pub id: String,
    pub channel: String,
    pub destination: String,
    pub verified: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize, FromRow)]
pub struct DeliveryLog {
    pub id: String,
    pub event_id: String,
    pub endpoint_id: Option<String>,
    pub channel_id: Option<String>,
    pub attempts: i64,
    pub status: String,
    pub next_attempt_at: String,
    pub last_error: Option<String>,
    pub created_at: String,
    pub delivered_at: Option<String>,
}

#[derive(Deserialize)]
pub struct RegisterWebhookRequest {
    pub url: String,
    pub event_types: Vec<String>,
}

#[derive(Serialize)]
pub struct RegisteredWebhook {
    pub id: String,
    pub secret: String,
    pub event_types: Vec<String>,
}

#[derive(Deserialize)]
pub struct RegisterChannelRequest {
    pub channel: String,
    pub destination: String,
}

#[derive(Deserialize)]
pub struct VerifyChannelRequest {
    pub code: String,
}

#[derive(Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
}

#[derive(Serialize)]
pub struct CreatedApiKey {
    pub id: String,
    pub key: String,
}

/// Authenticated API-key extractor. The raw key is never stored; lookups are
/// always against its SHA-256 digest in `api_keys`.
#[derive(Debug)]
pub struct ApiKeyUser(pub String);

#[axum::async_trait]
impl FromRequestParts<AppState> for ApiKeyUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let unauthorized = || AppError::new(StatusCode::UNAUTHORIZED, "missing or invalid API key");
        let key = parts
            .headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(unauthorized)?;
        if key.len() > 256 || key.is_empty() {
            return Err(unauthorized());
        }
        let hash = hex_encode(&sha256(key.as_bytes()));
        let owner: Option<String> =
            sqlx::query_scalar("SELECT wallet_address FROM api_keys WHERE key_hash = ?")
                .bind(hash)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| db_error("validate API key", e))?;
        owner.map(ApiKeyUser).ok_or_else(unauthorized)
    }
}

async fn create_webhook(
    state: &AppState,
    owner: &str,
    request: RegisterWebhookRequest,
) -> Result<RegisteredWebhook, AppError> {
    validate_destination(&request.url).await?;
    if request.event_types.is_empty()
        || request.event_types.len() > EVENT_TYPES.len()
        || request.event_types.iter().any(|e| !allowed_event(e))
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "event_types must contain one or more supported event types",
        ));
    }
    let mut event_types = request.event_types;
    event_types.sort();
    event_types.dedup();
    let id = uuid::Uuid::new_v4().to_string();
    let secret = random_hex(32);
    let event_types_json = serde_json::to_string(&event_types).unwrap();
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, wallet_address, url, secret, event_types)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(owner)
    .bind(request.url)
    .bind(&secret)
    .bind(event_types_json)
    .execute(&state.db)
    .await
    .map_err(|e| db_error("register webhook endpoint", e))?;
    Ok(RegisteredWebhook {
        id,
        secret,
        event_types,
    })
}

pub async fn register_webhook(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
    AppJson(request): AppJson<RegisterWebhookRequest>,
) -> Result<Json<RegisteredWebhook>, AppError> {
    Ok(Json(create_webhook(&state, &owner, request).await?))
}

pub async fn register_webhook_api_key(
    State(state): State<AppState>,
    ApiKeyUser(owner): ApiKeyUser,
    AppJson(request): AppJson<RegisterWebhookRequest>,
) -> Result<Json<RegisteredWebhook>, AppError> {
    Ok(Json(create_webhook(&state, &owner, request).await?))
}

pub async fn list_webhooks(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
) -> Result<Json<Vec<WebhookEndpoint>>, AppError> {
    let rows = sqlx::query_as::<_, WebhookEndpoint>(
        "SELECT id, url, event_types, active, created_at FROM webhook_endpoints
         WHERE wallet_address = ? ORDER BY created_at DESC",
    )
    .bind(owner)
    .fetch_all(&state.db)
    .await
    .map_err(|e| db_error("list webhook endpoints", e))?;
    Ok(Json(rows))
}

pub async fn delete_webhook(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    sqlx::query(
        "UPDATE delivery_attempts SET status = 'failed', last_error = 'webhook endpoint deleted'
         WHERE endpoint_id = ? AND status = 'pending'
           AND EXISTS (SELECT 1 FROM webhook_endpoints w
                       WHERE w.id = delivery_attempts.endpoint_id AND w.wallet_address = ?)",
    )
    .bind(&id)
    .bind(&owner)
    .execute(&state.db)
    .await
    .map_err(|e| db_error("mark endpoint deliveries unavailable", e))?;
    let result = sqlx::query("DELETE FROM webhook_endpoints WHERE id = ? AND wallet_address = ?")
        .bind(id)
        .bind(owner)
        .execute(&state.db)
        .await
        .map_err(|e| db_error("delete webhook endpoint", e))?;
    if result.rows_affected() == 0 {
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            "webhook endpoint not found",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn register_channel(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
    AppJson(request): AppJson<RegisterChannelRequest>,
) -> Result<Json<Channel>, AppError> {
    validate_channel_destination(&request.channel, &request.destination)?;
    let id = uuid::Uuid::new_v4().to_string();
    let code = random_hex(8);
    let hash = hex_encode(&sha256(code.as_bytes()));
    let expiry = timestamp(now_unix() + VERIFY_TTL_SECS);

    sqlx::query(
        "INSERT INTO delivery_channels
            (id, wallet_address, channel, destination, verification_hash, verification_expires_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(wallet_address, channel, destination) DO UPDATE SET
            verified = 0, verification_hash = excluded.verification_hash,
            verification_attempts = 0,
            verification_expires_at = excluded.verification_expires_at",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&request.channel)
    .bind(&request.destination)
    .bind(hash)
    .bind(expiry)
    .execute(&state.db)
    .await
    .map_err(|e| db_error("register notification channel", e))?;
    send_channel_verification(&request.channel, &request.destination, &code).await?;

    let channel = sqlx::query_as::<_, Channel>(
        "SELECT id, channel, destination, verified, created_at FROM delivery_channels
         WHERE wallet_address = ? AND channel = ? AND destination = ?",
    )
    .bind(owner)
    .bind(request.channel)
    .bind(request.destination)
    .fetch_one(&state.db)
    .await
    .map_err(|e| db_error("load notification channel", e))?;
    Ok(Json(channel))
}

pub async fn verify_channel(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
    Path(id): Path<String>,
    AppJson(request): AppJson<VerifyChannelRequest>,
) -> Result<Json<Channel>, AppError> {
    let hash = hex_encode(&sha256(request.code.as_bytes()));
    let now = timestamp(now_unix());
    let attempted = sqlx::query(
        "UPDATE delivery_channels SET verification_attempts = verification_attempts + 1
         WHERE id = ? AND wallet_address = ? AND verified = 0
           AND verification_expires_at >= ? AND verification_attempts < 5",
    )
    .bind(&id)
    .bind(&owner)
    .bind(timestamp(now_unix()))
    .execute(&state.db)
    .await
    .map_err(|e| db_error("record notification channel verification attempt", e))?;
    if attempted.rows_affected() == 0 {
        return Err(AppError::new(
            StatusCode::UNAUTHORIZED,
            "invalid, expired, or rate-limited channel verification code",
        ));
    }
    let result = sqlx::query(
        "UPDATE delivery_channels SET verified = 1, verification_hash = NULL,
                verification_expires_at = NULL
         WHERE id = ? AND wallet_address = ? AND verified = 0
           AND verification_hash = ? AND verification_expires_at >= ?",
    )
    .bind(&id)
    .bind(&owner)
    .bind(hash)
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(|e| db_error("verify notification channel", e))?;
    if result.rows_affected() == 0 {
        return Err(AppError::new(
            StatusCode::UNAUTHORIZED,
            "invalid or expired channel verification code",
        ));
    }
    let channel = sqlx::query_as::<_, Channel>(
        "SELECT id, channel, destination, verified, created_at FROM delivery_channels
         WHERE id = ? AND wallet_address = ?",
    )
    .bind(id)
    .bind(owner)
    .fetch_one(&state.db)
    .await
    .map_err(|e| db_error("load verified notification channel", e))?;
    Ok(Json(channel))
}

pub async fn list_channels(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
) -> Result<Json<Vec<Channel>>, AppError> {
    let rows = sqlx::query_as::<_, Channel>(
        "SELECT id, channel, destination, verified, created_at FROM delivery_channels
         WHERE wallet_address = ? ORDER BY created_at DESC",
    )
    .bind(owner)
    .fetch_all(&state.db)
    .await
    .map_err(|e| db_error("list notification channels", e))?;
    Ok(Json(rows))
}

pub async fn delete_channel(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    sqlx::query(
        "UPDATE delivery_attempts SET status = 'failed', last_error = 'notification channel deleted'
         WHERE channel_id = ? AND status = 'pending'
           AND EXISTS (SELECT 1 FROM delivery_channels c
                       WHERE c.id = delivery_attempts.channel_id AND c.wallet_address = ?)",
    )
        .bind(&id)
        .bind(&owner)
        .execute(&state.db)
        .await
        .map_err(|e| db_error("mark channel deliveries unavailable", e))?;
    let result = sqlx::query("DELETE FROM delivery_channels WHERE id = ? AND wallet_address = ?")
        .bind(id)
        .bind(owner)
        .execute(&state.db)
        .await
        .map_err(|e| db_error("delete notification channel", e))?;
    if result.rows_affected() == 0 {
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            "notification channel not found",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_api_key(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
    AppJson(request): AppJson<CreateApiKeyRequest>,
) -> Result<Json<CreatedApiKey>, AppError> {
    let name = request.name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "name must be 1-80 characters",
        ));
    }
    let key = format!("znt_{}", random_hex(32));
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO api_keys (id, wallet_address, name, key_hash) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(owner)
        .bind(name)
        .bind(hex_encode(&sha256(key.as_bytes())))
        .execute(&state.db)
        .await
        .map_err(|e| db_error("create API key", e))?;
    Ok(Json(CreatedApiKey { id, key }))
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let result = sqlx::query("DELETE FROM api_keys WHERE id = ? AND wallet_address = ?")
        .bind(id)
        .bind(owner)
        .execute(&state.db)
        .await
        .map_err(|e| db_error("delete API key", e))?;
    if result.rows_affected() == 0 {
        return Err(AppError::new(StatusCode::NOT_FOUND, "API key not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_delivery_logs(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
) -> Result<Json<Vec<DeliveryLog>>, AppError> {
    let rows = sqlx::query_as::<_, DeliveryLog>(
        "SELECT da.id, da.event_id, da.endpoint_id, da.channel_id, da.attempts, da.status,
                da.next_attempt_at, da.last_error, da.created_at, da.delivered_at
         FROM delivery_attempts da JOIN delivery_events e ON e.id = da.event_id
         WHERE e.wallet_address = ? ORDER BY da.created_at DESC LIMIT 200",
    )
    .bind(owner)
    .fetch_all(&state.db)
    .await
    .map_err(|e| db_error("list delivery logs", e))?;
    Ok(Json(rows))
}

pub async fn replay_delivery(
    State(state): State<AppState>,
    AuthUser(owner): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    replay_for_owner(&state, &owner, &id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn replay_delivery_api_key(
    State(state): State<AppState>,
    ApiKeyUser(owner): ApiKeyUser,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    replay_for_owner(&state, &owner, &id).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn replay_for_owner(state: &AppState, owner: &str, id: &str) -> Result<(), AppError> {
    let result = sqlx::query(
        "UPDATE delivery_attempts SET status = 'pending', attempts = 0, last_error = NULL,
                created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                next_attempt_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ? AND EXISTS (
             SELECT 1 FROM delivery_events e
             WHERE e.id = delivery_attempts.event_id AND e.wallet_address = ?
         ) AND (
             EXISTS (SELECT 1 FROM webhook_endpoints w WHERE w.id = delivery_attempts.endpoint_id
                     AND w.wallet_address = ?)
             OR EXISTS (SELECT 1 FROM delivery_channels c WHERE c.id = delivery_attempts.channel_id
                        AND c.wallet_address = ?)
         )",
    )
    .bind(id)
    .bind(owner)
    .bind(owner)
    .bind(owner)
    .execute(&state.db)
    .await
    .map_err(|e| db_error("replay delivery", e))?;
    if result.rows_affected() == 0 {
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            "delivery log not found",
        ));
    }
    Ok(())
}

/// Persist an event and enqueue a delivery for each matching verified
/// channel and active webhook. This is a trusted service API for event
/// producers, not a public handler; `wallet_address` must come from the
/// application-owned alert/position record.
pub async fn emit_event(
    state: &AppState,
    wallet_address: &str,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<usize, AppError> {
    if !NOTIFICATION_EVENT_TYPES.contains(&event_type) {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "unsupported delivery event type",
        ));
    }
    let body = serde_json::to_vec(&payload)
        .map_err(|_| AppError::new(StatusCode::BAD_REQUEST, "event payload is not valid JSON"))?;
    if body.len() > MAX_EVENT_PAYLOAD_BYTES {
        return Err(AppError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "event payload exceeds 30 KiB",
        ));
    }
    let webhooks: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, event_types FROM webhook_endpoints WHERE wallet_address = ? AND active = 1",
    )
    .bind(wallet_address)
    .fetch_all(&state.db)
    .await
    .map_err(|e| db_error("find webhook endpoints", e))?;
    let channels: Vec<(String,)> = sqlx::query_as(
        "SELECT c.id FROM delivery_channels c WHERE c.wallet_address = ? AND c.verified = 1",
    )
    .bind(wallet_address)
    .fetch_all(&state.db)
    .await
    .map_err(|e| db_error("find verified notification channels", e))?;

    let mut matching_webhooks = Vec::new();
    for (endpoint_id, event_types) in webhooks {
        match serde_json::from_str::<Vec<String>>(&event_types) {
            Ok(types) if types.iter().any(|e| e == event_type) => {
                matching_webhooks.push(endpoint_id)
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, endpoint_id, "stored webhook event types are invalid");
            }
        }
    }
    if matching_webhooks.is_empty() && channels.is_empty() {
        return Ok(0);
    }

    let event_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO delivery_events (id, wallet_address, event_type, payload) VALUES (?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(wallet_address)
    .bind(event_type)
    .bind(String::from_utf8(body).expect("serialized JSON is valid UTF-8"))
    .execute(&state.db)
    .await
    .map_err(|e| db_error("store delivery event", e))?;

    let mut count = 0;
    for endpoint_id in matching_webhooks {
        enqueue(state, &event_id, Some(&endpoint_id), None).await?;
        count += 1;
    }
    for (channel_id,) in channels {
        enqueue(state, &event_id, None, Some(&channel_id)).await?;
        count += 1;
    }
    Ok(count)
}

async fn enqueue(
    state: &AppState,
    event_id: &str,
    endpoint_id: Option<&str>,
    channel_id: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO delivery_attempts (id, event_id, endpoint_id, channel_id)
         VALUES (?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(event_id)
    .bind(endpoint_id)
    .bind(channel_id)
    .execute(&state.db)
    .await
    .map_err(|e| db_error("queue delivery", e))?;
    Ok(())
}

#[derive(FromRow)]
struct DueDelivery {
    id: String,
    event_id: String,
    attempts: i64,
    event_type: String,
    payload: String,
    url: Option<String>,
    secret: Option<String>,
    channel: Option<String>,
    destination: Option<String>,
}

/// Send up to `limit` due deliveries. The caller can schedule this from an
/// application-owned interval worker. Failures remain visible in the logs
/// and retry with capped exponential backoff (maximum delay 24 hours).
pub async fn process_due_deliveries(state: &AppState, limit: i64) -> Result<u64, AppError> {
    sqlx::query(
        "UPDATE delivery_attempts SET status = 'failed', last_error = 'retry window expired'
         WHERE status = 'pending'
           AND created_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-24 hours')",
    )
    .execute(&state.db)
    .await
    .map_err(|e| db_error("expire delivery retry windows", e))?;
    let rows = sqlx::query_as::<_, DueDelivery>(
        "SELECT da.id, da.event_id, da.attempts,
                e.event_type, e.payload, w.url, w.secret, c.channel, c.destination
         FROM delivery_attempts da
         JOIN delivery_events e ON e.id = da.event_id
         LEFT JOIN webhook_endpoints w ON w.id = da.endpoint_id
         LEFT JOIN delivery_channels c ON c.id = da.channel_id
         WHERE da.status = 'pending' AND da.next_attempt_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         ORDER BY da.created_at LIMIT ?",
    )
    .bind(limit.clamp(1, 100))
    .fetch_all(&state.db)
    .await
    .map_err(|e| db_error("load due deliveries", e))?;

    let mut delivered = 0;
    for row in rows {
        let result = if let (Some(url), Some(secret)) = (&row.url, &row.secret) {
            send_webhook(url, secret, &row.event_id, &row.event_type, &row.payload).await
        } else if let (Some(channel), Some(destination)) = (&row.channel, &row.destination) {
            send_notification(channel, destination, &row.event_type, &row.payload).await
        } else {
            Err("delivery destination no longer exists".to_string())
        };
        match result {
            Ok(()) => {
                sqlx::query(
                    "UPDATE delivery_attempts SET status = 'delivered', attempts = attempts + 1,
                        delivered_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), last_error = NULL
                     WHERE id = ?",
                )
                .bind(&row.id)
                .execute(&state.db)
                .await
                .map_err(|e| db_error("record successful delivery", e))?;
                delivered += 1;
            }
            Err(error) => {
                let attempts = row.attempts + 1;
                let delay = retry_delay(attempts);
                let next = timestamp(now_unix() + delay);
                tracing::warn!(
                    delivery_id = %row.id,
                    event_id = %row.event_id,
                    attempts,
                    error = %error,
                    retry_in_seconds = delay,
                    "notification delivery failed; scheduled retry"
                );
                sqlx::query(
                    "UPDATE delivery_attempts SET attempts = ?, last_error = ?, next_attempt_at = ?
                     WHERE id = ?",
                )
                .bind(attempts)
                .bind(error.chars().take(500).collect::<String>())
                .bind(next)
                .bind(&row.id)
                .execute(&state.db)
                .await
                .map_err(|e| db_error("record failed delivery", e))?;
            }
        }
    }
    Ok(delivered)
}

async fn send_webhook(
    url: &str,
    secret: &str,
    event_id: &str,
    event_type: &str,
    payload: &str,
) -> Result<(), String> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err("event payload exceeds 32 KiB".into());
    }
    let target = validate_destination(url).await.map_err(|e| e.message)?;
    let time = now_unix();
    let envelope = serde_json::json!({
        "id": event_id,
        "type": event_type,
        "created_at": timestamp(time),
        "data": serde_json::from_str::<serde_json::Value>(payload).map_err(|e| e.to_string())?,
    });
    let body = serde_json::to_vec(&envelope).map_err(|e| e.to_string())?;
    if body.len() > MAX_PAYLOAD_BYTES {
        return Err("webhook request exceeds 32 KiB".into());
    }
    let signature = sign_payload(secret.as_bytes(), time, &body);
    let timestamp_header = time.to_string();
    let signature_header = format!("sha256={signature}");
    let code = curl_post(
        &target,
        &body,
        &[
            ("Content-Type", "application/json"),
            ("X-Zenith-Event", event_type),
            ("X-Zenith-Delivery", event_id),
            ("X-Zenith-Timestamp", &timestamp_header),
            ("X-Zenith-Signature", &signature_header),
        ],
        None,
    )
    .await?;
    if !(200..300).contains(&code) {
        return Err(format!("endpoint returned HTTP {code}"));
    }
    Ok(())
}

#[derive(Clone)]
struct SafeTarget {
    url: String,
    host: String,
    port: u16,
    pinned_ip: Option<IpAddr>,
}

async fn validate_destination(url: &str) -> Result<SafeTarget, AppError> {
    if url.len() > 2048
        || url.bytes().any(|b| b <= 0x20 || b == 0x7f)
        || url.contains('#')
        || !url.starts_with("https://")
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "webhook URL must be an HTTPS URL without fragments or whitespace",
        ));
    }
    let rest = &url["https://".len()..];
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.contains('%') {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid webhook URL authority",
        ));
    }
    let (host, port) = if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or_else(|| AppError::new(StatusCode::BAD_REQUEST, "invalid IPv6 URL host"))?;
        let host = &authority[1..end];
        let suffix = &authority[end + 1..];
        let port = if suffix.is_empty() {
            443
        } else {
            suffix
                .strip_prefix(':')
                .and_then(|v| v.parse::<u16>().ok())
                .ok_or_else(|| AppError::new(StatusCode::BAD_REQUEST, "invalid webhook port"))?
        };
        (host.to_string(), port)
    } else {
        let mut pieces = authority.split(':');
        let host = pieces.next().unwrap_or("");
        let port = match pieces.next() {
            Some(port) => port
                .parse::<u16>()
                .map_err(|_| AppError::new(StatusCode::BAD_REQUEST, "invalid webhook port"))?,
            None => 443,
        };
        if pieces.next().is_some() {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "IPv6 URL hosts must be bracketed",
            ));
        }
        (host.to_string(), port)
    };
    if port != 443
        || host.is_empty()
        || (!host.parse::<IpAddr>().is_ok()
            && (host.len() > 253
                || host.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || !label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        || label.starts_with('-')
                        || label.ends_with('-')
                })))
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "webhook URL must use a valid host and standard HTTPS port",
        ));
    }
    let addresses: Vec<IpAddr> = if let Ok(ip) = host.parse() {
        vec![ip]
    } else {
        tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| AppError::new(StatusCode::BAD_REQUEST, "webhook host did not resolve"))?
            .map(|a| a.ip())
            .collect()
    };
    if addresses.is_empty() || addresses.iter().any(|ip| !is_public_ip(*ip)) {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "webhook host resolves to a private, loopback, link-local, or reserved address",
        ));
    }
    let pinned_ip = if host.parse::<IpAddr>().is_ok() {
        None
    } else {
        addresses.first().copied()
    };
    Ok(SafeTarget {
        url: url.to_string(),
        host,
        port,
        pinned_ip,
    })
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || b == 168))
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19 || b == 51))
                || (a == 203 && b == 0 && c == 113)
                || a >= 224)
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let segments = ip.segments();
            (0x2000..=0x3fff).contains(&segments[0])
                && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
        }
    }
}

async fn curl_post(
    target: &SafeTarget,
    body: &[u8],
    headers: &[(&str, &str)],
    bearer: Option<&str>,
) -> Result<u16, String> {
    if body.len() > MAX_PAYLOAD_BYTES {
        return Err("outbound request exceeds 32 KiB".into());
    }
    let mut cmd = tokio::process::Command::new("curl");
    cmd.arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg(MAX_HTTP_SECONDS)
        .arg("--connect-timeout")
        .arg("3")
        .arg("--max-filesize")
        .arg(MAX_PAYLOAD_BYTES.to_string())
        .arg("--max-redirs")
        .arg("0")
        .arg("--proto")
        .arg("=https")
        .arg("--noproxy")
        .arg("*")
        .arg("--proxy")
        .arg("")
        .arg("--output")
        .arg("/dev/null")
        .arg("--write-out")
        .arg("%{http_code}");
    if let Some(ip) = target.pinned_ip {
        let address = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        cmd.arg("--resolve")
            .arg(format!("{}:{}:{}", target.host, target.port, address));
    }
    for (name, value) in headers {
        cmd.arg("--header").arg(format!("{name}: {value}"));
    }
    if let Some(token) = bearer {
        cmd.arg("--header")
            .arg(format!("Authorization: Bearer {token}"));
    }
    let output = cmd
        .arg("--data-binary")
        .arg("@-")
        .arg(&target.url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start curl provider: {e}"))?;
    let mut child = output;
    use tokio::io::AsyncWriteExt;
    let mut stdin = child.stdin.take().ok_or("curl stdin unavailable")?;
    stdin.write_all(body).await.map_err(|e| e.to_string())?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .map_err(|_| "provider request exceeded 10 seconds".to_string())?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("HTTP provider request failed (transport error or non-2xx response)".into());
    }
    let code = String::from_utf8_lossy(&output.stdout)
        .parse::<u16>()
        .map_err(|_| "provider returned an invalid HTTP status".to_string())?;
    Ok(code)
}

fn validate_channel_destination(channel: &str, destination: &str) -> Result<(), AppError> {
    let valid = match channel {
        "email" => {
            destination.len() <= 254
                && destination.contains('@')
                && !destination.chars().any(char::is_whitespace)
        }
        "telegram" | "discord" => {
            !destination.is_empty()
                && destination.len() <= 128
                && destination.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    };
    if !valid {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "channel must be email, telegram, or discord with a valid destination",
        ));
    }
    Ok(())
}

async fn send_channel_verification(
    channel: &str,
    destination: &str,
    code: &str,
) -> Result<(), AppError> {
    let text = format!("Your Zenith verification code is {code}. It expires in 15 minutes.");
    send_channel_text(
        channel,
        destination,
        "Verify your Zenith notification channel",
        &text,
    )
    .await
    .map_err(|e| {
        tracing::warn!(channel, error = %e, "notification-channel verification send failed");
        AppError::new(
            StatusCode::BAD_GATEWAY,
            "channel verification message could not be sent; check provider configuration",
        )
    })
}

async fn send_notification(
    channel: &str,
    destination: &str,
    event_type: &str,
    payload: &str,
) -> Result<(), String> {
    let text = payload.chars().take(4_000).collect::<String>();
    let text = if payload.chars().count() > 4_000 {
        format!("{text}\n[notification payload truncated]")
    } else {
        text
    };
    send_channel_text(
        channel,
        destination,
        &format!("Zenith notification: {event_type}"),
        &text,
    )
    .await
}

async fn send_channel_text(
    channel: &str,
    destination: &str,
    subject: &str,
    text: &str,
) -> Result<(), String> {
    match channel {
        "email" => {
            let url = std::env::var("DELIVERY_EMAIL_API_URL")
                .map_err(|_| "DELIVERY_EMAIL_API_URL is not configured".to_string())?;
            let token = std::env::var("DELIVERY_EMAIL_API_TOKEN")
                .map_err(|_| "DELIVERY_EMAIL_API_TOKEN is not configured".to_string())?;
            let target = validate_destination(&url).await.map_err(|e| e.message)?;
            let sender = std::env::var("DELIVERY_EMAIL_FROM")
                .map_err(|_| "DELIVERY_EMAIL_FROM is not configured".to_string())?;
            let body = serde_json::to_vec(&serde_json::json!({
                "from": sender, "to": destination, "subject": subject, "text": text
            }))
            .map_err(|e| e.to_string())?;
            let status = curl_post(
                &target,
                &body,
                &[("Content-Type", "application/json")],
                Some(&token),
            )
            .await?;
            if !(200..300).contains(&status) {
                return Err(format!("email provider returned HTTP {status}"));
            }
            Ok(())
        }
        "telegram" => {
            let token = std::env::var("TELEGRAM_BOT_TOKEN")
                .map_err(|_| "TELEGRAM_BOT_TOKEN is not configured".to_string())?;
            let text = format!("{subject}\n{text}");
            let body = serde_json::to_vec(&serde_json::json!({
                "chat_id": destination, "text": text
            }))
            .map_err(|e| e.to_string())?;
            let target = trusted_provider_target(
                &format!("https://api.telegram.org/bot{token}/sendMessage"),
                "api.telegram.org",
            )
            .await?;
            let status = curl_post(
                &target,
                &body,
                &[("Content-Type", "application/json")],
                None,
            )
            .await?;
            if !(200..300).contains(&status) {
                return Err(format!("Telegram API returned HTTP {status}"));
            }
            Ok(())
        }
        "discord" => {
            let token = std::env::var("DISCORD_BOT_TOKEN")
                .map_err(|_| "DISCORD_BOT_TOKEN is not configured".to_string())?;
            let dm_body = serde_json::to_vec(&serde_json::json!({"recipient_id": destination}))
                .map_err(|e| e.to_string())?;
            let dm = trusted_provider_target(
                "https://discord.com/api/v10/users/@me/channels",
                "discord.com",
            )
            .await?;
            let channel_response = curl_json_post(&dm, &dm_body, "Bot", &token).await?;
            let channel_id = channel_response
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or("Discord did not return a DM channel id")?;
            let message_target = trusted_provider_target(
                &format!("https://discord.com/api/v10/channels/{channel_id}/messages"),
                "discord.com",
            )
            .await?;
            let message_body = serde_json::to_vec(&serde_json::json!({
                "content": format!("{subject}\n{text}")
            }))
            .map_err(|e| e.to_string())?;
            let status = curl_post(
                &message_target,
                &message_body,
                &[("Content-Type", "application/json")],
                Some(&format!("Bot {token}")),
            )
            .await?;
            if !(200..300).contains(&status) {
                return Err(format!("Discord API returned HTTP {status}"));
            }
            Ok(())
        }
        _ => Err("unknown notification channel".into()),
    }
}

async fn curl_json_post(
    target: &SafeTarget,
    body: &[u8],
    auth_scheme: &str,
    token: &str,
) -> Result<serde_json::Value, String> {
    let mut cmd = tokio::process::Command::new("curl");
    cmd.arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg(MAX_HTTP_SECONDS)
        .arg("--connect-timeout")
        .arg("3")
        .arg("--max-filesize")
        .arg(MAX_PAYLOAD_BYTES.to_string())
        .arg("--max-redirs")
        .arg("0")
        .arg("--proto")
        .arg("=https")
        .arg("--noproxy")
        .arg("*")
        .arg("--proxy")
        .arg("")
        .arg("--header")
        .arg("Content-Type: application/json")
        .arg("--header")
        .arg(format!("Authorization: {auth_scheme} {token}"));
    if let Some(ip) = target.pinned_ip {
        let ip = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        cmd.arg("--resolve")
            .arg(format!("{}:{}:{ip}", target.host, target.port));
    }
    cmd.arg("--data-binary")
        .arg("@-")
        .arg(&target.url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start curl provider: {e}"))?;
    use tokio::io::AsyncWriteExt;
    let mut stdin = child.stdin.take().ok_or("curl stdin unavailable")?;
    stdin.write_all(body).await.map_err(|e| e.to_string())?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .map_err(|_| "provider request exceeded 10 seconds".to_string())?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("provider request failed (transport error or non-2xx response)".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("invalid provider response: {e}"))
}

async fn trusted_provider_target(url: &str, host: &str) -> Result<SafeTarget, String> {
    let addresses: Vec<IpAddr> = tokio::net::lookup_host((host, 443))
        .await
        .map_err(|e| format!("provider DNS lookup failed: {e}"))?
        .map(|a| a.ip())
        .collect();
    if addresses.is_empty() || addresses.iter().any(|ip| !is_public_ip(*ip)) {
        return Err("provider host resolved to a non-public IP address".into());
    }
    Ok(SafeTarget {
        url: url.to_string(),
        host: host.to_string(),
        port: 443,
        pinned_ip: addresses.first().copied(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_and_hmac_match_standard_vectors() {
        assert_eq!(
            hex_encode(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sign_payload(&[0x0b; 20], 1_234_567_890, b"Hi There"),
            "e43b7d5c8e6bde6117f997972c8e0f8725283ae870677f4bc99004884c07425d"
        );
    }

    #[test]
    fn blocks_private_reserved_and_non_global_addresses() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.1.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.51.100.1",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[tokio::test]
    async fn rejects_non_https_and_nonstandard_ports() {
        assert!(validate_destination("http://example.com/hook")
            .await
            .is_err());
        assert!(validate_destination("https://example.com:8443/hook")
            .await
            .is_err());
        assert!(validate_destination("https://user@example.com/hook")
            .await
            .is_err());
        assert!(validate_destination("https://127.0.0.1/hook")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn trusted_events_queue_only_matching_hooks_and_verified_channels() {
        let path =
            std::env::temp_dir().join(format!("zenith-delivery-test-{}.db", uuid::Uuid::new_v4()));
        let db = crate::db::init_pool(&format!("sqlite://{}", path.display())).await;
        let state = AppState::new(db);
        sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('GWALLET')")
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO delivery_channels (id, wallet_address, channel, destination, verified)
             VALUES ('c1', 'GWALLET', 'email', 'owner@example.com', 1)",
        )
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO webhook_endpoints (id, wallet_address, url, secret, event_types)
             VALUES ('w1', 'GWALLET', 'https://example.com/hook', 'secret', '[\"alert_triggered\"]')",
        )
        .execute(&state.db)
        .await
        .unwrap();

        assert_eq!(
            emit_event(
                &state,
                "GWALLET",
                "alert_triggered",
                serde_json::json!({"alert_id":"a1"})
            )
            .await
            .unwrap(),
            2
        );
        assert_eq!(
            emit_event(
                &state,
                "GWALLET",
                "margin_call",
                serde_json::json!({"amount":10})
            )
            .await
            .unwrap(),
            1
        );
        let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_attempts")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(attempts, 3);

        state.db.close().await;
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn delivery_retries_stop_after_twenty_four_hours() {
        let path =
            std::env::temp_dir().join(format!("zenith-delivery-test-{}.db", uuid::Uuid::new_v4()));
        let db = crate::db::init_pool(&format!("sqlite://{}", path.display())).await;
        let state = AppState::new(db);
        sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('GWALLET')")
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO delivery_events (id, wallet_address, event_type, payload)
             VALUES ('e1', 'GWALLET', 'alert_triggered', '{}')",
        )
        .execute(&state.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO delivery_attempts (id, event_id, channel_id, created_at)
             VALUES ('d1', 'e1', 'c1',
                     strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-25 hours'))",
        )
        .execute(&state.db)
        .await
        .unwrap();

        assert_eq!(process_due_deliveries(&state, 10).await.unwrap(), 0);
        let status: String =
            sqlx::query_scalar("SELECT status FROM delivery_attempts WHERE id = 'd1'")
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(status, "failed");

        state.db.close().await;
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn exponential_backoff_starts_at_one_minute_and_caps_at_a_day() {
        assert_eq!(retry_delay(1), 60);
        assert_eq!(retry_delay(2), 120);
        assert_eq!(retry_delay(12), 86_400);
        assert_eq!(retry_delay(100), 86_400);
    }
}
