//! SVI (Stochastic Volatility Inspired) volatility surface.
//!
//! This module provides a per-underlying, per-expiry volatility surface built
//! from SVI raw-parameter slices. Slices are interpolated across expiries in
//! total variance, which keeps the surface free of calendar arbitrage when the
//! individual slices are monotone in total variance.
//!
//! The surface is intentionally a pure data structure: pricing code receives it
//! by reference (typically via `AppState` behind an `ArcSwap`) so readers never
//! block. Calibration inputs may come from configuration or a fixture; sourcing
//! live option quotes is out of scope.

use std::collections::BTreeMap;

/// SVI raw parameters for a single expiry slice.
///
/// The implied total variance is
/// `w(k) = a + b * (rho * (k - m) + sqrt((k - m)^2 + sigma^2))`
/// where `k = ln(strike / forward)` is the log-moneyness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SviParams {
    pub a: f64,
    pub b: f64,
    pub rho: f64,
    pub m: f64,
    pub sigma: f64,
}

impl SviParams {
    /// Total implied variance at log-moneyness `k`.
    pub fn total_variance(&self, k: f64) -> f64 {
        let d = k - self.m;
        self.a + self.b * (self.rho * d + (d * d + self.sigma * self.sigma).sqrt())
    }

    /// Implied volatility at log-moneyness `k` for time to expiry `t_years`.
    pub fn vol(&self, k: f64, t_years: f64) -> f64 {
        if t_years <= 0.0 {
            return 0.0;
        }
        let w = self.total_variance(k);
        if w <= 0.0 {
            return 0.0;
        }
        (w / t_years).sqrt()
    }

    /// Validate the slice against the no-butterfly-arbitrage conditions
    /// (Gatheral & Jacquier). Returns `false` when the parameters are
    /// inadmissible.
    ///
    /// Conditions enforced:
    /// * `b >= 0`, `sigma > 0`, `|rho| < 1`
    /// * `a + b * sigma * sqrt(1 - rho^2) >= 0` (non-negative total variance)
    /// * the Durrleman function `g(k) >= 0` for all `k` (no butterfly arbitrage)
    pub fn is_arbitrage_free(&self) -> bool {
        if !(self.b >= 0.0) || !(self.sigma > 0.0) || self.rho.abs() >= 1.0 {
            return false;
        }
        if self.a + self.b * self.sigma * (1.0 - self.rho * self.rho).sqrt() < 0.0 {
            return false;
        }
        // Durrleman's condition: g(k) >= 0 everywhere. We sample a dense grid
        // covering the practically relevant wings; the function is smooth and
        // its minimum is captured well within this range.
        let mut k = -5.0;
        while k <= 5.0 {
            if self.durrleman_g(k) < -1e-9 {
                return false;
            }
            k += 0.01;
        }
        true
    }

    /// Durrleman's function `g(k)`; non-negative iff the slice is free of
    /// butterfly arbitrage.
    pub fn durrleman_g(&self, k: f64) -> f64 {
        let d = k - self.m;
        let r = (d * d + self.sigma * self.sigma).sqrt();
        let w = self.a + self.b * (self.rho * d + r);
        if w <= 0.0 {
            return -1.0;
        }
        let w_prime = self.b * (self.rho + d / r);
        let w_second = self.b * self.sigma * self.sigma / (r * r * r);
        let term = 1.0 - k * w_prime / (2.0 * w);
        term * term - (w_prime * w_prime / 4.0) * (1.0 / w + 0.25) + w_second / 2.0
    }
}

/// A single calibrated expiry slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SviSlice {
    /// Time to expiry in years.
    pub t_years: f64,
    pub params: SviParams,
}

/// A calibrated SVI volatility surface for one underlying.
///
/// Slices are stored sorted by expiry. Interpolation across expiries is done in
/// total variance, which preserves the absence of calendar arbitrage when the
/// slices are monotone in total variance.
#[derive(Debug, Clone, Default)]
pub struct VolSurface {
    slices: Vec<SviSlice>,
}

impl VolSurface {
    /// Build a surface from slices. Slices are sorted by expiry and validated
    /// for total-variance monotonicity (no calendar arbitrage). Returns `None`
    /// when the slices are empty or violate the no-arbitrage conditions.
    pub fn new(mut slices: Vec<SviSlice>) -> Option<Self> {
        if slices.is_empty() {
            return None;
        }
        slices.sort_by(|a, b| a.t_years.partial_cmp(&b.t_years).unwrap_or(std::cmp::Ordering::Equal));
        for s in &slices {
            if !(s.t_years > 0.0) || !s.params.is_arbitrage_free() {
                return None;
            }
        }
        if !Self::is_calendar_arbitrage_free(&slices) {
            return None;
        }
        Some(Self { slices })
    }

    /// Check that total variance is non-decreasing in expiry at every sampled
    /// log-moneyness (no calendar arbitrage).
    fn is_calendar_arbitrage_free(slices: &[SviSlice]) -> bool {
        if slices.len() < 2 {
            return true;
        }
        let mut k = -5.0;
        while k <= 5.0 {
            let mut prev = f64::NEG_INFINITY;
            for s in slices {
                let w = s.params.total_variance(k);
                if w < prev - 1e-9 {
                    return false;
                }
                prev = w;
            }
            k += 0.05;
        }
        true
    }

    /// The calibrated slices, sorted by expiry.
    pub fn slices(&self) -> &[SviSlice] {
        &self.slices
    }

    /// Implied volatility at `strike` for time to expiry `t_years`.
    ///
    /// The forward is approximated by `spot` (zero rates/carry), so
    /// log-moneyness is `ln(strike / spot)`. Total variance is interpolated
    /// linearly across expiries and extrapolated flat per unit time beyond the
    /// first and last slice.
    pub fn vol(&self, strike: f64, spot: f64, t_years: f64) -> f64 {
        if self.slices.is_empty() || strike <= 0.0 || spot <= 0.0 || t_years <= 0.0 {
            return 0.0;
        }
        let k = (strike / spot).ln();
        let w = self.total_variance(k, t_years);
        if w <= 0.0 {
            return 0.0;
        }
        (w / t_years).sqrt()
    }

    /// Interpolate total variance across expiries at log-moneyness `k`.
    fn total_variance(&self, k: f64, t_years: f64) -> f64 {
        let first = &self.slices[0];
        let last = &self.slices[self.slices.len() - 1];

        // Flat total variance per unit time beyond the ends of the term
        // structure: w(t) = w(t_edge) * t / t_edge.
        if t_years <= first.t_years {
            return first.params.total_variance(k) * t_years / first.t_years;
        }
        if t_years >= last.t_years {
            return last.params.total_variance(k) * t_years / last.t_years;
        }

        // Linear interpolation in total variance between bracketing slices.
        for pair in self.slices.windows(2) {
            let (lo, hi) = (&pair[0], &pair[1]);
            if t_years >= lo.t_years && t_years <= hi.t_years {
                let w_lo = lo.params.total_variance(k);
                let w_hi = hi.params.total_variance(k);
                let span = hi.t_years - lo.t_years;
                if span <= 0.0 {
                    return w_lo;
                }
                let frac = (t_years - lo.t_years) / span;
                return w_lo + frac * (w_hi - w_lo);
            }
        }
        last.params.total_variance(k)
    }

    /// Build a strike x expiry implied-volatility grid for the API.
    ///
    /// `strikes` and `expiries` are the grid axes; the returned map is keyed by
    /// expiry (years) and holds the IV at each strike.
    pub fn iv_grid(&self, spot: f64, strikes: &[f64], expiries: &[f64]) -> BTreeMap<String, Vec<f64>> {
        let mut grid = BTreeMap::new();
        for &t in expiries {
            let row: Vec<f64> = strikes.iter().map(|&s| self.vol(s, spot, t)).collect();
            grid.insert(format!("{t:.6}"), row);
        }
        grid
    }
}

/// Fit an SVI slice to a set of `(strike, implied_vol)` points using the
/// Nelder-Mead simplex method.
///
/// `spot` is used to convert strikes to log-moneyness and `t_years` is the time
/// to expiry. Returns `None` when the fit fails to converge or produces
/// parameters that violate the no-arbitrage conditions; callers should keep the
/// previous parameters in that case.
///
pub fn calibrate_slice(
    spot: f64,
    t_years: f64,
    quotes: &[(f64, f64)],
) -> Option<SviParams> {
    if quotes.len() < 5 || spot <= 0.0 || t_years <= 0.0 {
        return None;
    }

    // Initial guess: flat ATM vol with mild skew.
    let atm = quotes
        .iter()
        .map(|&(k, v)| (k, v))
        .min_by(|a, b| (a.0 - spot).abs().partial_cmp(&(b.0 - spot).abs()).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, v)| v)
        .unwrap_or(0.5);
    let w_atm = (atm * atm * t_years).max(1e-6);
    let mut x = [w_atm * 0.5, 0.1, -0.1, 0.0, 0.1];

    let objective = |p: &[f64; 5]| -> f64 {
        let params = SviParams { a: p[0], b: p[1], rho: p[2], m: p[3], sigma: p[4] };
        if !(params.b >= 0.0) || !(params.sigma > 1e-6) || params.rho.abs() >= 0.999 {
            return f64::INFINITY;
        }
        let mut err = 0.0;
        for &(strike, iv) in quotes {
            let k = (strike / spot).ln();
            let model = params.vol(k, t_years);
            let d = model - iv;
            err += d * d;
        }
        err
    };

    let fitted = nelder_mead(&mut x, &objective, 2000, 1e-10)?;
    let params = SviParams { a: fitted[0], b: fitted[1], rho: fitted[2], m: fitted[3], sigma: fitted[4] };
    if params.is_arbitrage_free() {
        Some(params)
    } else {
        None
    }
}

/// Nelder-Mead simplex minimisation over a 5-dimensional parameter vector.
fn nelder_mead(
    x0: &mut [f64; 5],
    f: &dyn Fn(&[f64; 5]) -> f64,
    max_iter: usize,
    tol: f64,
) -> Option<[f64; 5]> {
    const N: usize = 5;
    let mut simplex: Vec<[f64; 5]> = Vec::with_capacity(N + 1);
    simplex.push(*x0);
    for i in 0..N {
        let mut p = *x0;
        let step = if p[i].abs() > 1e-6 { p[i] * 0.1 } else { 0.05 };
        p[i] += step;
        simplex.push(p);
    }

    let mut values: Vec<f64> = simplex.iter().map(|p| f(p)).collect();

    for _ in 0..max_iter {
        // Order by objective value.
        let mut order: Vec<usize> = (0..=N).collect();
        order.sort_by(|&a, &b| values[a].partial_cmp(&values[b]).unwrap_or(std::cmp::Ordering::Equal));
        let sorted_simplex: Vec<[f64; 5]> = order.iter().map(|&i| simplex[i]).collect();
        let sorted_values: Vec<f64> = order.iter().map(|&i| values[i]).collect();
        simplex = sorted_simplex;
        values = sorted_values;

        if (values[N] - values[0]).abs() < tol {
            break;
        }

        // Centroid of all but the worst point.
        let mut centroid = [0.0f64; N];
        for p in simplex.iter().take(N) {
            for i in 0..N {
                centroid[i] += p[i] / N as f64;
            }
        }

        // Reflection.
        let reflected = combine(&centroid, &simplex[N], 1.0);
        let f_reflected = f(&reflected);

        if f_reflected < values[0] {
            // Expansion.
            let expanded = combine(&centroid, &simplex[N], 2.0);
            let f_expanded = f(&expanded);
            if f_expanded < f_reflected {
                simplex[N] = expanded;
                values[N] = f_expanded;
            } else {
                simplex[N] = reflected;
                values[N] = f_reflected;
            }
        } else if f_reflected < values[N - 1] {
            simplex[N] = reflected;
            values[N] = f_reflected;
        } else {
            // Contraction.
            let contracted = if f_reflected < values[N] {
                combine(&centroid, &simplex[N], 0.5)
            } else {
                combine(&centroid, &simplex[N], -0.5)
            };
            let f_contracted = f(&contracted);
            if f_contracted < values[N] {
                simplex[N] = contracted;
                values[N] = f_contracted;
            } else {
                // Shrink toward the best point.
                for i in 1..=N {
                    for j in 0..N {
                        simplex[i][j] = simplex[0][j] + 0.5 * (simplex[i][j] - simplex[0][j]);
                    }
                    values[i] = f(&simplex[i]);
                }
            }
        }
    }

    let mut best = 0;
    for i in 1..=N {
        if values[i] < values[best] {
            best = i;
        }
    }
    if values[best].is_finite() {
        *x0 = simplex[best];
        Some(simplex[best])
    } else {
        None
    }
}

/// `centroid + coeff * (centroid - worst)` for the Nelder-Mead reflection,
/// expansion and contraction steps.
fn combine(centroid: &[f64; 5], worst: &[f64; 5], coeff: f64) -> [f64; 5] {
    let mut out = [0.0f64; 5];
    for i in 0..5 {
        out[i] = centroid[i] + coeff * (centroid[i] - worst[i]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_slice(t: f64, vol: f64) -> SviSlice {
        // A flat slice: b = 0, a = vol^2 * t.
        SviSlice {
            t_years: t,
            params: SviParams { a: vol * vol * t, b: 0.0, rho: 0.0, m: 0.0, sigma: 0.1 },
        }
    }

    #[test]
    fn flat_surface_recovers_vol() {
        let surface = VolSurface::new(vec![flat_slice(1.0, 0.5)]).unwrap();
        let v = surface.vol(100.0, 100.0, 1.0);
        assert!((v - 0.5).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn interpolates_total_variance_across_expiries() {
        let surface = VolSurface::new(vec![flat_slice(1.0, 0.4), flat_slice(2.0, 0.4)]).unwrap();
        // Total variance is linear in t, so vol is constant at 0.4.
        let v = surface.vol(100.0, 100.0, 1.5);
        assert!((v - 0.4).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn extrapolates_flat_total_variance_per_unit_time() {
        let surface = VolSurface::new(vec![flat_slice(1.0, 0.5)]).unwrap();
        // Beyond the last slice, w(t) = w(1) * t, so vol stays 0.5.
        let v = surface.vol(100.0, 100.0, 3.0);
        assert!((v - 0.5).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn rejects_negative_total_variance() {
        let bad = SviParams { a: -1.0, b: 0.0, rho: 0.0, m: 0.0, sigma: 0.1 };
        assert!(!bad.is_arbitrage_free());
    }

    #[test]
    fn rejects_calendar_arbitrage() {
        // Second slice has lower total variance than the first.
        let s1 = flat_slice(1.0, 0.5);
        let s2 = flat_slice(2.0, 0.2);
        assert!(VolSurface::new(vec![s1, s2]).is_none());
    }

    #[test]
    fn calibration_round_trips_known_parameters() {
        let truth = SviParams { a: 0.04, b: 0.1, rho: -0.3, m: 0.0, sigma: 0.2 };
        let spot = 100.0;
        let t = 1.0;
        let quotes: Vec<(f64, f64)> = (-5..=5)
            .map(|i| {
                let strike = spot * (1.0 + i as f64 * 0.05);
                let k = (strike / spot).ln();
                (strike, truth.vol(k, t))
            })
            .collect();
        let fitted = calibrate_slice(spot, t, &quotes).expect("calibration should converge");
        assert!(fitted.is_arbitrage_free());
        // The fit should reproduce the quotes closely.
        for &(strike, iv) in &quotes {
            let k = (strike / spot).ln();
            assert!((fitted.vol(k, t) - iv).abs() < 1e-3, "iv mismatch at {strike}");
        }
    }

    #[test]
    fn iv_grid_has_expected_shape() {
        let surface = VolSurface::new(vec![flat_slice(1.0, 0.5)]).unwrap();
        let grid = surface.iv_grid(100.0, &[90.0, 100.0, 110.0], &[1.0]);
        assert_eq!(grid.len(), 1);
        assert_eq!(grid.values().next().unwrap().len(), 3);
    }
}
