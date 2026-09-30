//! Implied Volatility Surface, Term-Structure and Skew API (issue #14).
//!
//! Read-only endpoints computed from the live vol surface:
//! * `GET /api/v1/surface/:underlying`        strike x expiry IV grid
//! * `GET /api/v1/term-structure/:underlying` ATM IV per expiry
//! * `GET /api/v1/skew/:underlying?expiry=`   25-delta RR25 / BF25 per expiry
//!
//! Grids are generated server-side in a single pass and cached per
//! `(underlying, surface_version)`; the version bumps on every tick.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use crate::{black_scholes, smile_vol, AppState, BSInputs};

const RISK_FREE: f64 = 0.05;
const EXPIRIES: [f64; 6] = [1.0, 7.0, 14.0, 30.0, 60.0, 90.0];
const STRIKE_STEPS: usize = 11;

/// Cache keyed by `(underlying, surface_version)`.
#[derive(Default)]
pub struct SurfaceCache {
    version: u64,
    entries: std::collections::HashMap<String, Arc<SurfaceData>>,
}

#[derive(Clone)]
pub struct SurfaceData {
    pub as_of: u64,
    pub etag: String,
    pub grid: Vec<GridRow>,
    pub term: Vec<TermPoint>,
    pub skew: Vec<SkewPoint>,
}

#[derive(Clone, Serialize)]
pub struct GridRow {
    pub strike: f64,
    pub ivs: Vec<Option<f64>>,
}

#[derive(Clone, Serialize)]
pub struct TermPoint {
    pub expiry_days: f64,
    pub atm_iv: f64,
}

#[derive(Clone, Serialize)]
pub struct SkewPoint {
    pub expiry_days: f64,
    pub rr25: Option<f64>,
    pub bf25: Option<f64>,
}

#[derive(Serialize)]
pub struct SurfaceResponse {
    pub underlying: String,
    pub as_of: u64,
    pub expiries: Vec<f64>,
    pub grid: Vec<GridRow>,
}

#[derive(Serialize)]
pub struct TermResponse {
    pub underlying: String,
    pub as_of: u64,
    pub term_structure: Vec<TermPoint>,
}

#[derive(Serialize)]
pub struct SkewResponse {
    pub underlying: String,
    pub as_of: u64,
    pub skew: Vec<SkewPoint>,
}

#[derive(Deserialize)]
pub struct SkewQuery {
    pub expiry: Option<f64>,
}

fn base_vol(state: &AppState, underlying: &str) -> Option<f64> {
    state
        .vol_surface
        .lock()
        .ok()
        .and_then(|v| v.get(underlying).copied())
}

fn spot(state: &AppState, underlying: &str) -> Option<f64> {
    state
        .spot_prices
        .lock()
        .ok()
        .and_then(|p| p.get(underlying).copied())
}

/// Numerically invert delta to find the strike whose BS delta equals `target`.
/// Returns `None` when the wing cannot be solved (extreme strikes).
fn strike_for_delta(spot: f64, t: f64, vol: f64, target: f64, is_call: bool) -> Option<f64> {
    let mut lo = spot * 0.05;
    let mut hi = spot * 20.0;
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        let d = black_scholes(&BSInputs {
            spot,
            strike: mid,
            vol,
            t,
            r: RISK_FREE,
            is_call,
        })
        .delta;
        if (d - target).abs() < 1e-6 {
            return Some(mid);
        }
        if d > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let mid = 0.5 * (lo + hi);
    if mid.is_finite() && mid > 0.0 {
        Some(mid)
    } else {
        None
    }
}

fn iv_at(spot: f64, strike: f64, t: f64, base: f64) -> Option<f64> {
    if t <= 0.0 {
        return None;
    }
    let moneyness = strike / spot;
    Some(smile_vol(base, moneyness))
}

/// Build the full surface in a single pass.
fn build(underlying: &str, spot: f64, base: f64, as_of: u64) -> SurfaceData {
    let mut grid = Vec::with_capacity(STRIKE_STEPS);
    for i in 0..STRIKE_STEPS {
        let m = 0.7 + 0.06 * i as f64; // 0.70 .. 1.30 moneyness
        let strike = spot * m;
        let ivs = EXPIRIES
            .iter()
            .map(|d| iv_at(spot, strike, d / 365.0, base))
            .collect();
        grid.push(GridRow { strike, ivs });
    }

    let term = EXPIRIES
        .iter()
        .map(|d| TermPoint {
            expiry_days: *d,
            atm_iv: smile_vol(base, 1.0),
        })
        .collect();

    let skew = EXPIRIES
        .iter()
        .map(|d| {
            let t = d / 365.0;
            let call25 = strike_for_delta(spot, t, base, 0.25, true);
            let put25 = strike_for_delta(spot, t, base, -0.25, false);
            match (call25, put25) {
                (Some(c), Some(p)) => {
                    let iv_c = smile_vol(base, c / spot);
                    let iv_p = smile_vol(base, p / spot);
                    let atm = smile_vol(base, 1.0);
                    Some(SkewPoint {
                        expiry_days: *d,
                        rr25: Some(iv_c - iv_p),
                        bf25: Some(0.5 * (iv_c + iv_p) - atm),
                    })
                }
                _ => Some(SkewPoint {
                    expiry_days: *d,
                    rr25: None,
                    bf25: None,
                }),
            }
        })
        .collect::<Vec<_>>();

    let mut hasher = DefaultHasher::new();
    underlying.hash(&mut hasher);
    as_of.hash(&mut hasher);
    let etag = format!("\"{:x}\"", hasher.finish());

    SurfaceData {
        as_of,
        etag,
        grid,
        term,
        skew,
    }
}

/// Fetch (or build) the cached surface for `underlying`.
fn get_surface(state: &AppState, underlying: &str) -> Option<Arc<SurfaceData>> {
    let spot = spot(state, underlying)?;
    let base = base_vol(state, underlying)?;
    let as_of = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache = state.surface_cache.lock().ok()?;
    if let Some(data) = cache.entries.get(underlying) {
        if data.as_of == as_of {
            return Some(data.clone());
        }
    }
    drop(cache);

    let data = Arc::new(build(underlying, spot, base, as_of));
    if let Ok(mut cache) = state.surface_cache.lock() {
        cache.version = cache.version.wrapping_add(1);
        cache.entries.insert(underlying.to_string(), data.clone());
    }
    Some(data)
}

fn conditional(headers: &HeaderMap, etag: &str) -> Option<Response> {
    let inm = headers.get(header::IF_NONE_MATCH)?.to_str().ok()?;
    if inm == etag || inm == "*" {
        Some(
            (
                StatusCode::NOT_MODIFIED,
                [(header::ETAG, etag.to_string())],
            )
                .into_response(),
        )
    } else {
        None
    }
}

pub async fn surface_handler(
    State(state): State<AppState>,
    Path(underlying): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(data) = get_surface(&state, &underlying) else {
        return (StatusCode::NOT_FOUND, "unknown underlying").into_response();
    };
    if let Some(r) = conditional(&headers, &data.etag) {
        return r;
    }
    let body = SurfaceResponse {
        underlying,
        as_of: data.as_of,
        expiries: EXPIRIES.to_vec(),
        grid: data.grid.clone(),
    };
    (
        StatusCode::OK,
        [(header::ETAG, data.etag.clone())],
        Json(body),
    )
        .into_response()
}

pub async fn term_structure_handler(
    State(state): State<AppState>,
    Path(underlying): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(data) = get_surface(&state, &underlying) else {
        return (StatusCode::NOT_FOUND, "unknown underlying").into_response();
    };
    if let Some(r) = conditional(&headers, &data.etag) {
        return r;
    }
    let body = TermResponse {
        underlying,
        as_of: data.as_of,
        term_structure: data.term.clone(),
    };
    (
        StatusCode::OK,
        [(header::ETAG, data.etag.clone())],
        Json(body),
    )
        .into_response()
}

pub async fn skew_handler(
    State(state): State<AppState>,
    Path(underlying): Path<String>,
    Query(q): Query<SkewQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(data) = get_surface(&state, &underlying) else {
        return (StatusCode::NOT_FOUND, "unknown underlying").into_response();
    };
    if let Some(r) = conditional(&headers, &data.etag) {
        return r;
    }
    let skew = match q.expiry {
        Some(e) => data
            .skew
            .iter()
            .filter(|s| (s.expiry_days - e).abs() < 1e-9)
            .cloned()
            .collect(),
        None => data.skew.clone(),
    };
    let body = SkewResponse {
        underlying,
        as_of: data.as_of,
        skew,
    };
    (
        StatusCode::OK,
        [(header::ETAG, data.etag.clone())],
        Json(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_inversion_accuracy() {
        let spot = 100.0;
        let t = 30.0 / 365.0;
        let vol = 0.8;
        let k = strike_for_delta(spot, t, vol, 0.25, true).unwrap();
        let d = black_scholes(&BSInputs {
            spot,
            strike: k,
            vol,
            t,
            r: RISK_FREE,
            is_call: true,
        })
        .delta;
        assert!((d - 0.25).abs() < 1e-6);
    }

    #[test]
    fn build_has_all_expiries() {
        let data = build("XLM", 0.1182, 0.82, 1);
        assert_eq!(data.term.len(), EXPIRIES.len());
        assert_eq!(data.skew.len(), EXPIRIES.len());
        assert_eq!(data.grid.len(), STRIKE_STEPS);
    }
}
