//! Term-structured risk-free rate and funding/dividend curves per underlying.
//!
//! Crypto options are typically priced off a funding- or basis-adjusted forward
//! rather than a flat zero rate. This module provides a [`RateCurve`] with
//! piecewise-linear interpolation over tenors, configured per underlying, and a
//! [`RateCurves`] registry that resolves the interpolated risk-free rate and
//! carry (`b = r - q`) for a given underlying and time-to-expiry.
//!
//! The curves are intentionally self-contained so they can be stored inside the
//! `MarketSnapshot` (see the ArcSwap issue) and swapped atomically with prices.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A single tenor point on a curve: `tenor` is expressed in years.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RatePoint {
    /// Time to expiry in years.
    pub tenor: f64,
    /// Continuously-compounded rate at this tenor.
    pub rate: f64,
}

/// Piecewise-linear curve over tenors.
///
/// Interpolation is linear between the two bracketing points; outside the
/// quoted range the nearest endpoint is held flat (no extrapolation). A curve
/// with a single point is treated as a flat rate. Negative rates are supported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateCurve {
    /// Tenor points, kept sorted by `tenor` on construction.
    points: Vec<RatePoint>,
}

impl RateCurve {
    /// Build a curve from tenor points. Points are sorted by tenor; duplicate
    /// tenors keep the last value supplied. An empty input yields a flat zero
    /// curve so callers always get a usable curve.
    pub fn new(mut points: Vec<RatePoint>) -> Self {
        points.sort_by(|a, b| {
            a.tenor
                .partial_cmp(&b.tenor)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if points.is_empty() {
            points.push(RatePoint {
                tenor: 0.0,
                rate: 0.0,
            });
        }
        Self { points }
    }

    /// A flat curve at the given continuously-compounded rate.
    pub fn flat(rate: f64) -> Self {
        Self {
            points: vec![RatePoint { tenor: 0.0, rate }],
        }
    }

    /// The tenor points backing this curve.
    pub fn points(&self) -> &[RatePoint] {
        &self.points
    }

    /// Interpolate the rate at `tenor` (years).
    ///
    /// A zero or negative tenor resolves to the shortest-tenor rate. Tenors
    /// beyond the last point hold the last rate flat.
    pub fn rate_at(&self, tenor: f64) -> f64 {
        let t = if tenor.is_finite() && tenor > 0.0 {
            tenor
        } else {
            0.0
        };

        let first = self.points[0];
        if t <= first.tenor {
            return first.rate;
        }

        let last = self.points[self.points.len() - 1];
        if t >= last.tenor {
            return last.rate;
        }

        for window in self.points.windows(2) {
            let (lo, hi) = (window[0], window[1]);
            if t >= lo.tenor && t <= hi.tenor {
                let span = hi.tenor - lo.tenor;
                if span <= 0.0 {
                    return hi.rate;
                }
                let w = (t - lo.tenor) / span;
                return lo.rate + w * (hi.rate - lo.rate);
            }
        }

        last.rate
    }
}

impl Default for RateCurve {
    /// Sensible default: a flat zero curve.
    fn default() -> Self {
        Self::flat(0.0)
    }
}

/// Per-underlying configuration of the risk-free curve and the funding or
/// dividend yield used to build the carry `b = r - q`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnderlyingRates {
    /// Term-structured risk-free rate.
    pub risk_free: RateCurve,
    /// Term-structured funding or dividend yield `q`.
    pub funding: RateCurve,
}

impl UnderlyingRates {
    /// Build a per-underlying config from explicit curves.
    pub fn new(risk_free: RateCurve, funding: RateCurve) -> Self {
        Self { risk_free, funding }
    }

    /// Resolve `(rate, carry)` for a given tenor, where `carry = r - q`.
    pub fn at(&self, tenor: f64) -> (f64, f64) {
        let r = self.risk_free.rate_at(tenor);
        let q = self.funding.rate_at(tenor);
        (r, r - q)
    }
}

impl Default for UnderlyingRates {
    fn default() -> Self {
        Self {
            risk_free: RateCurve::default(),
            funding: RateCurve::default(),
        }
    }
}

/// Registry of per-underlying rate curves with a global default fallback.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RateCurves {
    default: UnderlyingRates,
    per_underlying: HashMap<String, UnderlyingRates>,
}

impl RateCurves {
    /// Create an empty registry using the default (flat zero) curves.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the fallback curves used for underlyings without an explicit entry.
    pub fn set_default(&mut self, rates: UnderlyingRates) {
        self.default = rates;
    }

    /// Insert or replace the curves for a specific underlying.
    pub fn set(&mut self, underlying: &str, rates: UnderlyingRates) {
        self.per_underlying.insert(underlying.to_string(), rates);
    }

    /// Borrow the curves configured for an underlying, falling back to the
    /// default when none is registered.
    pub fn get(&self, underlying: &str) -> &UnderlyingRates {
        self.per_underlying
            .get(underlying)
            .unwrap_or(&self.default)
    }

    /// Resolve `(rate, carry)` for an underlying and tenor.
    pub fn resolve(&self, underlying: &str, tenor: f64) -> (f64, f64) {
        self.get(underlying).at(tenor)
    }
}

/// Hook for sourcing funding rates from external venues (e.g. perp funding).
///
/// Out of scope for the initial implementation: callers may provide an
/// implementation to refresh [`RateCurves`] from a live source.
pub trait FundingRateSource {
    /// Return the funding curve for an underlying, if available.
    fn funding_curve(&self, underlying: &str) -> Option<RateCurve>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_curve_returns_constant_rate() {
        let curve = RateCurve::flat(0.05);
        assert_eq!(curve.rate_at(0.0), 0.05);
        assert_eq!(curve.rate_at(1.0), 0.05);
        assert_eq!(curve.rate_at(10.0), 0.05);
    }

    #[test]
    fn piecewise_linear_interpolation() {
        let curve = RateCurve::new(vec![
            RatePoint { tenor: 0.0, rate: 0.02 },
            RatePoint { tenor: 1.0, rate: 0.04 },
            RatePoint { tenor: 2.0, rate: 0.06 },
        ]);
        assert!((curve.rate_at(0.5) - 0.03).abs() < 1e-12);
        assert!((curve.rate_at(1.5) - 0.05).abs() < 1e-12);
    }

    #[test]
    fn holds_flat_outside_range_and_handles_zero_tenor() {
        let curve = RateCurve::new(vec![
            RatePoint { tenor: 0.5, rate: 0.03 },
            RatePoint { tenor: 1.0, rate: 0.05 },
        ]);
        assert_eq!(curve.rate_at(0.0), 0.03);
        assert_eq!(curve.rate_at(-1.0), 0.03);
        assert_eq!(curve.rate_at(5.0), 0.05);
    }

    #[test]
    fn negative_rates_supported() {
        let curve = RateCurve::flat(-0.01);
        assert_eq!(curve.rate_at(1.0), -0.01);
    }

    #[test]
    fn carry_is_rate_minus_funding() {
        let rates = UnderlyingRates::new(RateCurve::flat(0.05), RateCurve::flat(0.02));
        let (r, b) = rates.at(1.0);
        assert!((r - 0.05).abs() < 1e-12);
        assert!((b - 0.03).abs() < 1e-12);
    }

    #[test]
    fn registry_falls_back_to_default() {
        let mut curves = RateCurves::new();
        curves.set_default(UnderlyingRates::new(RateCurve::flat(0.04), RateCurve::flat(0.0)));
        curves.set(
            "BTC",
            UnderlyingRates::new(RateCurve::flat(0.06), RateCurve::flat(0.01)),
        );
        assert!((curves.resolve("BTC", 1.0).0 - 0.06).abs() < 1e-12);
        assert!((curves.resolve("ETH", 1.0).0 - 0.04).abs() < 1e-12);
    }
}
