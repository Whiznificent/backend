use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortfolioSourceFilter {
    Onchain,
    Paper,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedPositionView {
    pub id: String,
    pub source: String, // "onchain" | "paper"
    pub underlying: String,
    pub strike: f64,
    pub expiry_days: f64,
    pub option_type: String, // "call" | "put"
    pub position_type: String, // "long" | "short"
    pub contracts: f64,
    pub entry_premium: Option<f64>, // None if unknown cost basis from transfer
    pub current_premium: f64,
    pub unrealized_pnl: Option<f64>,
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioSummary {
    pub total_positions: usize,
    pub total_delta: f64,
    pub total_gamma: f64,
    pub total_theta: f64,
    pub total_vega: f64,
    pub total_unrealized_pnl: f64,
    pub positions: Vec<UnifiedPositionView>,
}

pub struct PortfolioSyncService {
    cache: Arc<Mutex<HashMap<String, (PortfolioSummary, Instant)>>>,
    cache_ttl: Duration,
}

impl PortfolioSyncService {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_ttl: Duration::from_secs(10),
        }
    }

    pub fn get_cached(&self, wallet: &str) -> Option<PortfolioSummary> {
        let cache = self.cache.lock().unwrap();
        if let Some((summary, cached_at)) = cache.get(wallet) {
            if cached_at.elapsed() < self.cache_ttl {
                return Some(summary.clone());
            }
        }
        None
    }

    pub fn set_cached(&self, wallet: &str, summary: PortfolioSummary) {
        let mut cache = self.cache.lock().unwrap();
        cache.insert(wallet.to_string(), (summary, Instant::now()));
    }
}

impl Default for PortfolioSyncService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
pub struct PortfolioQuery {
    pub source: Option<PortfolioSourceFilter>,
}

pub async fn get_portfolio_handler(
    State(state): State<crate::AppState>,
    crate::auth::AuthUser(wallet_address): crate::auth::AuthUser,
    Query(q): Query<PortfolioQuery>,
) -> Result<Json<PortfolioSummary>, StatusCode> {
    let filter = q.source.unwrap_or(PortfolioSourceFilter::All);

    // Fetch paper positions from SQLite DB
    let rows: Vec<crate::models::Position> = sqlx::query_as(
        "SELECT * FROM positions WHERE wallet_address = ? AND status = 'open'",
    )
    .bind(&wallet_address)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let prices = state.spot_prices.lock().unwrap().clone();
    let vols = state.vol_surface.lock().unwrap().clone();

    let mut unified_positions = Vec::new();
    let mut total_delta = 0.0;
    let mut total_gamma = 0.0;
    let mut total_theta = 0.0;
    let mut total_vega = 0.0;
    let mut total_pnl = 0.0;

    if filter == PortfolioSourceFilter::Paper || filter == PortfolioSourceFilter::All {
        for pos in rows {
            let spot = prices.get(&pos.underlying).cloned().unwrap_or(100.0);
            let base_vol = vols.get(&pos.underlying).cloned().unwrap_or(0.8);
            let vol = crate::smile_vol(base_vol, pos.strike / spot);
            let bs = crate::black_scholes(&crate::BSInputs {
                spot,
                strike: pos.strike,
                vol,
                t: pos.expiry_days / 365.0,
                r: 0.05,
                is_call: pos.option_type == "call",
            });

            let mult = if pos.position_type == "long" { 1.0 } else { -1.0 };
            let pnl = (bs.premium - pos.entry_premium) * pos.contracts * mult;

            total_delta += bs.delta * pos.contracts * mult;
            total_gamma += bs.gamma * pos.contracts * mult;
            total_theta += bs.theta * pos.contracts * mult;
            total_vega += bs.vega * pos.contracts * mult;
            total_pnl += pnl;

            unified_positions.push(UnifiedPositionView {
                id: pos.id,
                source: "paper".into(),
                underlying: pos.underlying,
                strike: pos.strike,
                expiry_days: pos.expiry_days,
                option_type: pos.option_type,
                position_type: pos.position_type,
                contracts: pos.contracts,
                entry_premium: Some(pos.entry_premium),
                current_premium: bs.premium,
                unrealized_pnl: Some(pnl),
                delta: bs.delta * mult,
                gamma: bs.gamma * mult,
                theta: bs.theta * mult,
                vega: bs.vega * mult,
            });
        }
    }

    if filter == PortfolioSourceFilter::Onchain || filter == PortfolioSourceFilter::All {
        // Sample onchain position simulation
        let spot = prices.get("XLM").cloned().unwrap_or(0.12);
        let bs = crate::black_scholes(&crate::BSInputs {
            spot,
            strike: 0.12,
            vol: 0.82,
            t: 14.0 / 365.0,
            r: 0.05,
            is_call: true,
        });

        unified_positions.push(UnifiedPositionView {
            id: "ONCHAIN_SERIES_TOKEN_01".into(),
            source: "onchain".into(),
            underlying: "XLM".into(),
            strike: 0.12,
            expiry_days: 14.0,
            option_type: "call".into(),
            position_type: "long".into(),
            contracts: 500.0,
            entry_premium: None, // Transferred token with unknown cost basis
            current_premium: bs.premium,
            unrealized_pnl: None,
            delta: bs.delta,
            gamma: bs.gamma,
            theta: bs.theta,
            vega: bs.vega,
        });
    }

    let summary = PortfolioSummary {
        total_positions: unified_positions.len(),
        total_delta,
        total_gamma,
        total_theta,
        total_vega,
        total_unrealized_pnl: total_pnl,
        positions: unified_positions,
    };

    Ok(Json(summary))
}
