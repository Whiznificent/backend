use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};

use crate::error::{AppError, ValidatedJson};
use crate::{black_scholes, smile_vol, AppState, BSInputs, BSResult};

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct PriceSpec {
    #[validate(length(min = 1, max = 32))]
    pub underlying: String,
    #[validate(range(min = 0.000001, max = 1000000000.0))]
    pub strike: f64,
    #[validate(range(min = 0.000001, max = 3650.0))]
    pub expiry_days: f64,
    #[validate(custom(function = "valid_option_type"))]
    pub option_type: String,
}

fn valid_option_type(value: &str) -> Result<(), ValidationError> {
    if value == "call" || value == "put" {
        Ok(())
    } else {
        Err(ValidationError::new("invalid_option_type"))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct StrategySpecs {
    #[validate(length(max = 64))]
    pub id: Option<String>,
    #[validate(length(min = 1, max = 500))]
    pub legs: Vec<PriceSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[validate(schema(function = "validate_batch_size"))]
pub struct BatchPriceRequest {
    #[serde(default)]
    pub specs: Vec<PriceSpec>,
    #[serde(default)]
    pub strategies: Vec<StrategySpecs>,
}

fn validate_batch_size(request: &BatchPriceRequest) -> Result<(), ValidationError> {
    let count = request.specs.len() + request.strategies.iter().map(|s| s.legs.len()).sum::<usize>();
    if count > 0
        && count <= 500
        && request.strategies.iter().all(|strategy| !strategy.legs.is_empty())
        && request.specs.len() <= 500
        && request.strategies.len() <= 500
    {
        Ok(())
    } else {
        Err(ValidationError::new("max_batch_items"))
    }
}

#[derive(Clone, Serialize)]
pub struct BatchItem<T> {
    pub result: Option<T>,
    pub error: Option<BatchItemError>,
}

#[derive(Clone, Serialize)]
pub struct BatchItemError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Serialize)]
pub struct BatchStrategyResult {
    pub id: Option<String>,
    pub legs: Vec<BatchItem<BSResult>>,
}

#[derive(Serialize)]
pub struct BatchPriceResponse {
    pub specs: Vec<BatchItem<BSResult>>,
    pub strategies: Vec<BatchStrategyResult>,
}

fn price_spec(
    spec: &PriceSpec,
    prices: &std::collections::HashMap<String, f64>,
    vols: &std::collections::HashMap<String, f64>,
) -> Result<BSResult, BatchItemError> {
    let fail = |code, message: String| Err(BatchItemError { code, message });
    if spec.underlying.is_empty() || spec.underlying.len() > 32 {
        return fail("invalid_underlying", "underlying must contain 1 to 32 characters".into());
    }
    if !spec.strike.is_finite() || spec.strike <= 0.0 {
        return fail("invalid_strike", "strike must be a finite positive number".into());
    }
    if !spec.expiry_days.is_finite() || !(0.0..=3650.0).contains(&spec.expiry_days) || spec.expiry_days == 0.0 {
        return fail("invalid_expiry", "expiry_days must be greater than 0 and at most 3650".into());
    }
    let is_call = match spec.option_type.as_str() {
        "call" => true,
        "put" => false,
        _ => return fail("invalid_option_type", "option_type must be \"call\" or \"put\"".into()),
    };
    let Some(spot) = prices.get(&spec.underlying) else {
        return fail("unknown_underlying", format!("unknown underlying \"{}\"", spec.underlying));
    };
    let Some(base_vol) = vols.get(&spec.underlying) else {
        return fail("unknown_underlying", format!("unknown underlying \"{}\"", spec.underlying));
    };
    Ok(black_scholes(&BSInputs {
        spot: *spot,
        strike: spec.strike,
        vol: smile_vol(*base_vol, spec.strike / spot),
        t: spec.expiry_days / 365.0,
        r: 0.05,
        is_call,
    }))
}

fn eval_item(spec: PriceSpec, prices: &std::collections::HashMap<String, f64>, vols: &std::collections::HashMap<String, f64>) -> BatchItem<BSResult> {
    match price_spec(&spec, prices, vols) {
        Ok(result) => BatchItem { result: Some(result), error: None },
        Err(error) => BatchItem { result: None, error: Some(error) },
    }
}

pub async fn post_batch(
    State(state): State<AppState>,
    ValidatedJson(request): ValidatedJson<BatchPriceRequest>,
) -> Result<Json<BatchPriceResponse>, AppError> {
    let (prices, vols) = {
        let prices = state.spot_prices.lock().unwrap();
        let vols = state.vol_surface.lock().unwrap();
        (prices.clone(), vols.clone())
    };

    enum JobResult {
        Spec(usize, BatchItem<BSResult>),
        Strategy(usize, usize, BatchItem<BSResult>),
    }
    let mut jobs = tokio::task::JoinSet::new();
    let mut specs = Vec::with_capacity(request.specs.len());
    let mut strategies = Vec::with_capacity(request.strategies.len());
    for (index, spec) in request.specs.into_iter().enumerate() {
        let prices = prices.clone();
        let vols = vols.clone();
        specs.push(None);
        jobs.spawn_blocking(move || JobResult::Spec(index, eval_item(spec, &prices, &vols)));
    }
    for (strategy_index, strategy) in request.strategies.into_iter().enumerate() {
        let mut legs = Vec::with_capacity(strategy.legs.len());
        for (leg_index, spec) in strategy.legs.into_iter().enumerate() {
            let prices = prices.clone();
            let vols = vols.clone();
            legs.push(None);
            jobs.spawn_blocking(move || JobResult::Strategy(strategy_index, leg_index, eval_item(spec, &prices, &vols)));
        }
        strategies.push((strategy.id, legs));
    }

    while let Some(result) = jobs.join_next().await {
        match result {
            Ok(JobResult::Spec(index, item)) => {
                specs[index] = Some(item);
            }
            Ok(JobResult::Strategy(strategy, leg, item)) => {
                strategies[strategy].1[leg] = Some(item);
            }
            Err(error) => {
                tracing::error!(%error, "batch pricing worker failed");
                return Err(AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "batch pricing failed"));
            }
        }
    }

    Ok(Json(BatchPriceResponse {
        specs: specs.into_iter().map(Option::unwrap).collect(),
        strategies: strategies.into_iter().map(|(id, legs)| BatchStrategyResult {
            id,
            legs: legs.into_iter().map(Option::unwrap).collect(),
        }).collect(),
    }))
}
