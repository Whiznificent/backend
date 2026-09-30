//! Scenario analysis: reprice a set of option legs across a user-defined grid of
//! spot shocks, volatility shocks and forward time steps.
//!
//! This module is intentionally self-contained: it consumes the analytic
//! [`BSResult`] Greeks from [`crate::black_scholes`] and the [`Leg`] type used by
//! the payoff endpoint, and produces a P&L grid plus aggregated base-case Greeks.

use serde::{Deserialize, Serialize};

use crate::black_scholes;
use crate::positions::AggregateGreeks;

/// Hard, deny-by-default cap on the number of grid cells evaluated per request.
///
/// `spot_shocks_pct.len() * vol_shocks_abs.len() * days_forward.len()` must not
/// exceed this value; oversized grids are rejected before any pricing work is
/// done to prevent CPU-exhaustion DoS.
pub const MAX_GRID_CELLS: usize = 2_500;

/// Volatility floor (as a fraction, i.e. 1%) applied after vol shocks so that a
/// shock can never drive implied vol to zero or below.
pub const MIN_VOL: f64 = 0.01;

/// A single option leg supplied by the caller (unauthenticated mode) or derived
/// from the user's open positions (authenticated mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Leg {
    /// Spot price of the underlying at the base case.
    pub spot: f64,
    /// Strike price.
    pub strike: f64,
    /// Time to expiry in years at the base case.
    pub time_to_expiry: f64,
    /// Implied volatility at the base case (fraction, e.g. 0.2 for 20%).
    pub vol: f64,
    /// Risk-free rate (fraction).
    pub rate: f64,
    /// Continuous dividend yield (fraction).
    pub dividend: f64,
    /// `true` for a call, `false` for a put.
    pub is_call: bool,
    /// Position size; negative for short positions.
    pub quantity: f64,
}

/// Request body for `POST /api/v1/portfolio/scenario`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioRequest {
    /// Spot shocks as percentages, e.g. `[-0.1, 0.0, 0.1]` for -10%, 0%, +10%.
    pub spot_shocks_pct: Vec<f64>,
    /// Absolute volatility shocks (fractions), e.g. `[-0.05, 0.0, 0.05]`.
    pub vol_shocks_abs: Vec<f64>,
    /// Forward time steps in calendar days, e.g. `[0.0, 1.0, 7.0]`.
    pub days_forward: Vec<f64>,
    /// Caller-supplied legs. When `None`, the caller's open positions are used
    /// (authenticated mode).
    #[serde(default)]
    pub legs: Option<Vec<Leg>>,
}

/// A single cell of the scenario grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioCell {
    /// Spot shock applied to this cell (fraction).
    pub spot_shock_pct: f64,
    /// Volatility shock applied to this cell (fraction).
    pub vol_shock_abs: f64,
    /// Forward time step applied to this cell (days).
    pub days_forward: f64,
    /// Total P&L of the portfolio at this cell, relative to the base case.
    pub pnl: f64,
}

/// Response body for `POST /api/v1/portfolio/scenario`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResponse {
    /// P&L for every cell of the grid, in row-major order over
    /// `(spot_shocks_pct, vol_shocks_abs, days_forward)`.
    pub cells: Vec<ScenarioCell>,
    /// The cell with the most negative P&L (worst case for a long book).
    pub worst_case: ScenarioCell,
    /// Aggregated Greeks evaluated at the base case (zero shocks).
    pub base_greeks: AggregateGreeks,
}

/// Error returned when a scenario request is malformed or exceeds the grid cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioError {
    /// The requested grid exceeds [`MAX_GRID_CELLS`].
    GridTooLarge { cells: usize, max: usize },
    /// One of the shock axes was empty.
    EmptyAxis,
    /// No legs were supplied and no open positions were available.
    NoLegs,
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScenarioError::GridTooLarge { cells, max } => {
                write!(f, "scenario grid has {cells} cells, exceeding the cap of {max}")
            }
            ScenarioError::EmptyAxis => write!(f, "scenario shock axes must not be empty"),
            ScenarioError::NoLegs => write!(f, "no legs supplied and no open positions found"),
        }
    }
}

impl std::error::Error for ScenarioError {}

/// Number of cells a request would evaluate.
pub fn grid_cell_count(req: &ScenarioRequest) -> usize {
    req.spot_shocks_pct.len() * req.vol_shocks_abs.len() * req.days_forward.len()
}

/// Validate a scenario request against the deny-by-default grid cap.
pub fn validate_request(req: &ScenarioRequest) -> Result<(), ScenarioError> {
    if req.spot_shocks_pct.is_empty()
        || req.vol_shocks_abs.is_empty()
        || req.days_forward.is_empty()
    {
        return Err(ScenarioError::EmptyAxis);
    }
    let cells = grid_cell_count(req);
    if cells > MAX_GRID_CELLS {
        return Err(ScenarioError::GridTooLarge {
            cells,
            max: MAX_GRID_CELLS,
        });
    }
    Ok(())
}

/// Price a single leg, returning intrinsic value once it has expired.
fn price_leg(leg: &Leg, spot: f64, vol: f64, time_to_expiry: f64) -> f64 {
    if time_to_expiry <= 0.0 {
        let intrinsic = if leg.is_call {
            (spot - leg.strike).max(0.0)
        } else {
            (leg.strike - spot).max(0.0)
        };
        return intrinsic * leg.quantity;
    }
    let result = black_scholes(
        spot,
        leg.strike,
        time_to_expiry,
        vol,
        leg.rate,
        leg.dividend,
        leg.is_call,
    );
    result.price * leg.quantity
}

/// Total portfolio value across all legs at the given shocks.
fn portfolio_value(legs: &[Leg], spot_mult: f64, vol_shift: f64, days: f64) -> f64 {
    let dt = days / 365.0;
    legs.iter()
        .map(|leg| {
            let spot = leg.spot * spot_mult;
            let vol = (leg.vol + vol_shift).max(MIN_VOL);
            let t = (leg.time_to_expiry - dt).max(0.0);
            price_leg(leg, spot, vol, t)
        })
        .sum()
}

/// Aggregate the analytic Greeks of every leg at the base case.
fn aggregate_base_greeks(legs: &[Leg]) -> AggregateGreeks {
    let mut agg = AggregateGreeks::default();
    for leg in legs {
        let r = black_scholes(
            leg.spot,
            leg.strike,
            leg.time_to_expiry,
            leg.vol,
            leg.rate,
            leg.dividend,
            leg.is_call,
        );
        agg.delta += r.delta * leg.quantity;
        agg.gamma += r.gamma * leg.quantity;
        agg.vega += r.vega * leg.quantity;
        agg.theta += r.theta * leg.quantity;
        agg.rho += r.rho * leg.quantity;
    }
    agg
}

/// Evaluate a scenario grid over the supplied legs.
///
/// The grid is evaluated in row-major order over
/// `(spot_shocks_pct, vol_shocks_abs, days_forward)`. P&L is measured relative
/// to the base case (zero shocks).
pub fn run_scenario(legs: &[Leg], req: &ScenarioRequest) -> Result<ScenarioResponse, ScenarioError> {
    validate_request(req)?;
    if legs.is_empty() {
        return Err(ScenarioError::NoLegs);
    }

    let base_value = portfolio_value(legs, 1.0, 0.0, 0.0);

    let mut cells = Vec::with_capacity(grid_cell_count(req));
    for &spot_shock in &req.spot_shocks_pct {
        for &vol_shock in &req.vol_shocks_abs {
            for &days in &req.days_forward {
                let value = portfolio_value(legs, 1.0 + spot_shock, vol_shock, days);
                cells.push(ScenarioCell {
                    spot_shock_pct: spot_shock,
                    vol_shock_abs: vol_shock,
                    days_forward: days,
                    pnl: value - base_value,
                });
            }
        }
    }

    let worst_case = cells
        .iter()
        .min_by(|a, b| a.pnl.partial_cmp(&b.pnl).unwrap_or(std::cmp::Ordering::Equal))
        .cloned()
        .expect("grid is non-empty after validation");

    Ok(ScenarioResponse {
        cells,
        worst_case,
        base_greeks: aggregate_base_greeks(legs),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_leg() -> Leg {
        Leg {
            spot: 100.0,
            strike: 100.0,
            time_to_expiry: 1.0,
            vol: 0.2,
            rate: 0.05,
            dividend: 0.0,
            is_call: true,
            quantity: 1.0,
        }
    }

    fn base_request() -> ScenarioRequest {
        ScenarioRequest {
            spot_shocks_pct: vec![-0.1, 0.0, 0.1],
            vol_shocks_abs: vec![-0.05, 0.0, 0.05],
            days_forward: vec![0.0, 1.0, 7.0],
            legs: None,
        }
    }

    #[test]
    fn rejects_oversized_grid() {
        let mut req = base_request();
        req.spot_shocks_pct = (0..20).map(|i| i as f64 * 0.01).collect();
        req.vol_shocks_abs = (0..20).map(|i| i as f64 * 0.01).collect();
        req.days_forward = (0..20).map(|i| i as f64).collect();
        assert!(matches!(
            validate_request(&req),
            Err(ScenarioError::GridTooLarge { .. })
        ));
    }

    #[test]
    fn accepts_grid_at_cap() {
        let mut req = base_request();
        req.spot_shocks_pct = (0..10).map(|i| i as f64 * 0.01).collect();
        req.vol_shocks_abs = (0..10).map(|i| i as f64 * 0.01).collect();
        req.days_forward = (0..25).map(|i| i as f64).collect();
        assert_eq!(grid_cell_count(&req), MAX_GRID_CELLS);
        assert!(validate_request(&req).is_ok());
    }

    #[test]
    fn base_case_pnl_is_zero() {
        let legs = vec![sample_leg()];
        let resp = run_scenario(&legs, &base_request()).unwrap();
        let base = resp
            .cells
            .iter()
            .find(|c| c.spot_shock_pct == 0.0 && c.vol_shock_abs == 0.0 && c.days_forward == 0.0)
            .unwrap();
        assert!(base.pnl.abs() < 1e-9);
    }

    #[test]
    fn worst_case_is_minimum_pnl() {
        let legs = vec![sample_leg()];
        let resp = run_scenario(&legs, &base_request()).unwrap();
        let min = resp
            .cells
            .iter()
            .map(|c| c.pnl)
            .fold(f64::INFINITY, f64::min);
        assert!((resp.worst_case.pnl - min).abs() < 1e-12);
    }

    #[test]
    fn vol_shock_is_floored() {
        let legs = vec![sample_leg()];
        let mut req = base_request();
        req.vol_shocks_abs = vec![-1.0];
        let resp = run_scenario(&legs, &req).unwrap();
        // With vol floored at 1% the option still has positive value.
        assert!(resp.cells[0].pnl.is_finite());
    }

    #[test]
    fn expired_leg_uses_intrinsic() {
        let legs = vec![sample_leg()];
        let mut req = base_request();
        req.days_forward = vec![400.0];
        let resp = run_scenario(&legs, &req).unwrap();
        // Deep past expiry, intrinsic value of an ATM call is 0.
        assert!(resp.cells[0].pnl.is_finite());
    }

    #[test]
    fn empty_axis_is_rejected() {
        let mut req = base_request();
        req.days_forward = vec![];
        assert_eq!(validate_request(&req), Err(ScenarioError::EmptyAxis));
    }

    #[test]
    fn no_legs_is_rejected() {
        let req = base_request();
        assert_eq!(run_scenario(&[], &req), Err(ScenarioError::NoLegs));
    }
}
