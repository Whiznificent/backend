use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use redis::Script;
use prometheus::{IntCounterVec, Opts, Registry};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

use crate::AppState;

const GCRA_SCRIPT: &str = r#"
local t = redis.call('TIME')
local now = t[1] * 1000 + math.floor(t[2] / 1000)
local tat = tonumber(redis.call('GET', KEYS[1])) or now
local interval = tonumber(ARGV[1])
local burst = tonumber(ARGV[2])
local cost = tonumber(ARGV[3])
local candidate = math.max(now, tat) + cost * interval
local allowed = candidate <= now + burst * interval
local next_tat = allowed and candidate or tat
if allowed then
  redis.call('SET', KEYS[1], next_tat, 'PX', math.ceil((next_tat - now) + burst * interval))
end
local remaining = math.max(0, math.floor((now + burst * interval - next_tat) / interval))
local reset = math.ceil(math.max(0, next_tat - burst * interval - now) / 1000)
local retry = math.ceil(math.max(0, tat - (now + (burst - cost) * interval)) / 1000)
return {allowed and 1 or 0, remaining, reset, retry}
"#;

#[derive(Clone)]
pub struct RateLimiter {
    redis: Option<redis::Client>,
    fallback: Arc<Mutex<HashMap<String, Bucket>>>,
}

#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Registry>,
    requests: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Arc::new(Registry::new());
        let requests = IntCounterVec::new(
            Opts::new(
                "zenith_http_requests_total",
                "HTTP requests by method and response status",
            ),
            &["method", "status"],
        )
        .expect("valid HTTP request metric definition");
        registry
            .register(Box::new(requests.clone()))
            .expect("unique HTTP request metric");
        Self { registry, requests }
    }

    pub fn record(&self, method: &str, status: StatusCode) {
        self.requests
            .with_label_values(&[method, status.as_str()])
            .inc();
    }

    pub fn gather(&self) -> Vec<prometheus::proto::MetricFamily> {
        self.registry.gather()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
struct Bucket {
    theoretical_arrival_ms: i64,
    expires_at_ms: i64,
}

#[derive(Clone, Copy)]
struct Tier {
    limit: u32,
    burst: u32,
}

#[derive(Clone, Copy)]
struct Decision {
    allowed: bool,
    remaining: u32,
    reset_seconds: u64,
    retry_seconds: u64,
}

#[derive(Clone, Copy)]
enum PrincipalTier {
    Anonymous,
    Wallet,
    ApiKey,
}

impl PrincipalTier {
    fn limits(self) -> Tier {
        match self {
            Self::Anonymous => Tier {
                limit: 120,
                burst: 10,
            },
            Self::Wallet => Tier {
                limit: 300,
                burst: 40,
            },
            Self::ApiKey => Tier {
                limit: 1200,
                burst: 100,
            },
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Wallet => "wallet",
            Self::ApiKey => "api-key",
        }
    }
}

impl RateLimiter {
    pub fn new(redis_url: Option<&str>) -> Result<Self, redis::RedisError> {
        let redis = redis_url.map(redis::Client::open).transpose()?;
        Ok(Self {
            redis,
            fallback: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    async fn check(&self, key: &str, tier: Tier, cost: u32) -> Result<Decision, redis::RedisError> {
        let interval_ms = 60_000.0 / tier.limit as f64;
        if let Some(client) = &self.redis {
            let mut conn = client.get_multiplexed_async_connection().await?;
            let result: (u8, u32, u64, u64) = Script::new(GCRA_SCRIPT)
                .key(key)
                .arg(interval_ms)
                .arg(tier.burst)
                .arg(cost)
                .invoke_async(&mut conn)
                .await?;
            return Ok(Decision {
                allowed: result.0 == 1,
                remaining: result.1,
                reset_seconds: result.2,
                retry_seconds: result.3.max(1),
            });
        }
        Ok(self.check_local(key, tier, cost, interval_ms).await)
    }

    async fn check_local(&self, key: &str, tier: Tier, cost: u32, interval_ms: f64) -> Decision {
        let now = unix_millis();
        let interval = interval_ms.ceil() as i64;
        let burst_window = i64::from(tier.burst) * interval;
        let mut buckets = self.fallback.lock().await;
        buckets.retain(|_, bucket| bucket.expires_at_ms > now);

        let bucket = buckets.entry(key.to_owned()).or_insert(Bucket {
            theoretical_arrival_ms: now,
            expires_at_ms: now + burst_window,
        });
        let tat = bucket.theoretical_arrival_ms;
        let candidate = tat.max(now) + i64::from(cost) * interval;
        let allowed = candidate <= now + burst_window;
        let next_tat = if allowed { candidate } else { tat };
        bucket.theoretical_arrival_ms = next_tat;
        bucket.expires_at_ms = next_tat.max(now) + burst_window;

        Decision {
            allowed,
            remaining: ((now + burst_window - next_tat).max(0) / interval) as u32,
            reset_seconds: ((next_tat - burst_window - now).max(0) as u64).div_ceil(1000),
            retry_seconds: ((tat - (now + i64::from(tier.burst.saturating_sub(cost)) * interval))
                .max(0) as u64)
                .div_ceil(1000)
                .max(1),
        }
    }
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn route_cost(path: &str, method: &axum::http::Method) -> u32 {
    if path == "/api/v1/chain" {
        5
    } else if path == "/api/v1/portfolio/payoff" {
        4
    } else if path == "/api/v1/price" || path == "/api/v1/iv" {
        2
    } else if method == axum::http::Method::POST {
        2
    } else {
        1
    }
}

fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    for name in ["x-forwarded-for", "x-real-ip"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            if let Some(ip) = value
                .split(',')
                .next()
                .map(str::trim)
                .filter(|ip| !ip.is_empty())
            {
                return ip.to_owned();
            }
        }
    }
    if let Some(value) = headers
        .get("forwarded")
        .and_then(|value| value.to_str().ok())
    {
        for part in value.split(';') {
            if let Some(for_value) = part.trim().strip_prefix("for=") {
                return for_value.trim_matches('"').to_owned();
            }
        }
    }
    peer.map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

async fn principal(
    headers: HeaderMap,
    peer: Option<SocketAddr>,
    state: &AppState,
) -> Result<(PrincipalTier, String), Response> {
    if let Some(api_key) = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        return Ok((PrincipalTier::ApiKey, format!("api-key:{api_key}")));
    }

    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        let wallet = sqlx::query_scalar::<_, String>(
            "SELECT wallet_address FROM sessions WHERE token = ? AND expires_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(token)
        .fetch_optional(&state.db)
        .await;
        match wallet {
            Ok(Some(wallet)) => return Ok((PrincipalTier::Wallet, format!("wallet:{wallet}"))),
            Ok(None) => {}
            Err(error) => {
                tracing::error!(error = %error, "rate limiter could not resolve bearer principal");
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "principal lookup unavailable",
                )
                    .into_response());
            }
        }
    }

    Ok((
        PrincipalTier::Anonymous,
        format!("ip:{}", client_ip(&headers, peer)),
    ))
}

fn set_header(headers: &mut HeaderMap, name: &'static str, value: u64) {
    if let Ok(value) = HeaderValue::from_str(&value.to_string()) {
        headers.insert(name, value);
    }
}

pub async fn middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if matches!(path, "/health" | "/livez" | "/readyz" | "/metrics") {
        return next.run(request).await;
    }

    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let (tier, principal) = match principal(request.headers().clone(), peer, &state).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let limit = tier.limits();
    let cost = route_cost(path, request.method());
    let mut digest = Sha256::new();
    digest.update(tier.label().as_bytes());
    digest.update(b":");
    digest.update(principal.as_bytes());
    let key = format!(
        "zenith:rate:{}",
        data_encoding::HEXLOWER.encode(&digest.finalize())
    );

    let decision = match state.rate_limiter.check(&key, limit, cost).await {
        Ok(decision) => decision,
        Err(error) => {
            tracing::warn!(error = %error, "Redis rate limiter unavailable; using local GCRA fallback");
            state
                .rate_limiter
                .check_local(&key, limit, cost, 60_000.0 / limit.limit as f64)
                .await
        }
    };

    if !decision.allowed {
        let mut response = (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
        apply_headers(response.headers_mut(), limit, decision);
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(decision.retry_seconds.min(u64::from(u32::MAX)) as u32),
        );
        state.metrics.record(request.method().as_str(), response.status());
        return response;
    }

    let method = request.method().as_str().to_owned();
    let mut response = next.run(request).await;
    apply_headers(response.headers_mut(), limit, decision);
    state.metrics.record(&method, response.status());
    response
}

fn apply_headers(headers: &mut HeaderMap, tier: Tier, decision: Decision) {
    set_header(headers, "ratelimit-limit", u64::from(tier.limit));
    set_header(
        headers,
        "ratelimit-remaining",
        u64::from(decision.remaining),
    );
    set_header(headers, "ratelimit-reset", decision.reset_seconds);
}

use axum::response::IntoResponse;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn weighted_gcra_requests_consume_their_full_cost() {
        let limiter = RateLimiter::new(None).unwrap();
        let tier = Tier {
            limit: 60,
            burst: 5,
        };
        let first = limiter.check("test-key", tier, 4).await.unwrap();
        assert!(first.allowed);
        assert_eq!(first.remaining, 1);
        let second = limiter.check("test-key", tier, 2).await.unwrap();
        assert!(!second.allowed);
    }

    #[test]
    fn route_costs_are_weighted_by_expensive_operations() {
        assert_eq!(route_cost("/api/v1/chain", &axum::http::Method::GET), 5);
        assert_eq!(route_cost("/api/v1/price", &axum::http::Method::GET), 2);
        assert_eq!(
            route_cost("/api/v1/watchlist", &axum::http::Method::POST),
            2
        );
        assert_eq!(route_cost("/api/v1/spot", &axum::http::Method::GET), 1);
    }
}
