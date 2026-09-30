use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::{Json, Response};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::AppState;

#[derive(Default)]
pub struct VersionMetrics {
    v1_requests: AtomicU64,
    v2_requests: AtomicU64,
}

impl VersionMetrics {
    pub fn snapshot(&self) -> VersionUsage {
        VersionUsage {
            v1: self.v1_requests.load(Ordering::Relaxed),
            v2: self.v2_requests.load(Ordering::Relaxed),
        }
    }
}

#[derive(Serialize)]
pub struct VersionUsage {
    pub v1: u64,
    pub v2: u64,
}

pub async fn usage(State(state): State<AppState>) -> Json<VersionUsage> {
    Json(state.version_metrics.snapshot())
}

pub async fn track_usage_and_deprecations(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let is_v1 = path.starts_with("/api/v1/");
    if is_v1 {
        state
            .version_metrics
            .v1_requests
            .fetch_add(1, Ordering::Relaxed);
    } else if path.starts_with("/api/v2/") {
        state
            .version_metrics
            .v2_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    let deprecated = is_v1
        && state
            .config
            .deprecated_routes
            .iter()
            .any(|route| route == path);
    let mut response = next.run(request).await;
    if deprecated {
        if let Some(timestamp) = state.config.deprecation_timestamp {
            let value = format!("@{timestamp}");
            match HeaderValue::from_str(&value) {
                Ok(value) => {
                    response.headers_mut().insert("deprecation", value);
                }
                Err(error) => tracing::error!(%error, "invalid configured Deprecation header"),
            }
        }
        if let Some(sunset) = &state.config.sunset_date {
            match HeaderValue::from_str(sunset) {
                Ok(value) => {
                    response.headers_mut().insert("sunset", value);
                }
                Err(error) => tracing::error!(%error, "invalid configured Sunset header"),
            }
        }
    }
    response
}

#[derive(Serialize)]
pub struct SpotResponseV2 {
    pub assets: std::collections::HashMap<String, AssetQuoteV2>,
}

#[derive(Serialize)]
pub struct AssetQuoteV2 {
    pub price: f64,
    pub implied_vol: f64,
}

pub async fn get_spot_v2(State(state): State<AppState>) -> Json<SpotResponseV2> {
    let prices = state.spot_prices.lock().unwrap().clone();
    let vols = state.vol_surface.lock().unwrap().clone();
    let assets = prices
        .into_iter()
        .filter_map(|(symbol, price)| {
            vols.get(&symbol).map(|vol| {
                (
                    symbol,
                    AssetQuoteV2 {
                        price,
                        implied_vol: *vol,
                    },
                )
            })
        })
        .collect();
    Json(SpotResponseV2 { assets })
}
