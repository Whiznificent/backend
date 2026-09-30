//! SVI (Stochastic Volatility Inspired) raw-parameter slices and surfaces.
//!
//! A slice is parameterised by the raw SVI parameters `(a, b, rho, m, sigma)`
//! (Gatheral, "A parsimonious arbitrage-free implied volatility parameterization"):
//!
//! ```text
//! w(k) = a + b * ( rho * (k - m) + sqrt( (k - m)^2 + sigma^2 ) )
//! ```
//!
//! where `k = ln(K / F)` is log-moneyness and `w = sigma_bs^2 * t` is the total
//! implied variance. Working in total variance makes interpolation across
//! expiries linear and lets us enforce the no-calendar-arbitrage condition
//! directly.

use std::f64::consts::PI;

/// Raw SVI parameters for a single expiry slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SviParams {
    pub a: f64,
    pub b: f64,
    pub rho: f64,
    pub m: f64,
    pub sigma: f64,
}

impl SviParams {
    pub fn new(a: f64, b: f64, rho: f64, m: f64, sigma: f64) -> Self {
        Self { a, b, rho, m, sigma }
    }

    /// Total implied variance `w(k)` at log-moneyness `k`.
    pub fn total_variance(&self, k: f64) -> f64 {
        let d = k - self.m;
        self.a + self.b * (self.rho * d + (d * d + self.sigma * self.sigma).sqrt())
    }

    /// Implied Black-Scholes volatility at log-moneyness `k` for expiry `t`.
    pub fn implied_vol(&self, k: f64, t: f64) -> f64 {
        if t <= 0.0 {
            return 0.0;
        }
        let w = self.total_variance(k);
        if w <= 0.0 {
            return 0.0;
        }
        (w / t).sqrt()
    }

    /// Gatheral's no-butterfly-arbitrage conditions for a raw SVI slice.
    ///
    /// Returns `true` when the slice is free of butterfly arbitrage:
    /// `b >= 0`, `|rho| < 1`, `sigma > 0`, and the Durrleman condition
    /// `g(k) >= 0` for all `k` (checked on a dense grid plus the wings).
    pub fn is_butterfly_free(&self) -> bool {
        if !(self.b >= 0.0 && self.rho.abs() < 1.0 && self.sigma > 0.0) {
            return false;
        }
        // Durrleman's function g(k) must be non-negative everywhere.
        // Sample densely around the money and out into the wings.
        let mut k = -5.0;
        while k <= 5.0 {
            if self.durrleman_g(k) < -1e-9 {
                return false;
            }
            k += 0.01;
        }
        true
    }

    /// Durrleman's function `g(k)`; `g(k) >= 0` is equivalent to the
    /// density of the underlying being non-negative (no butterfly arbitrage).
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
        let g = term * term - (w_prime * w_prime / 4.0) * (1.0 / w + 0.25) + w_second / 2.0;
        g
    }
}

/// A single calibrated expiry slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SviSlice {
    /// Time to expiry in years.
    pub t: f64,
    pub params: SviParams,
}

/// A per-underlying SVI volatility surface: a set of slices ordered by expiry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VolSurface {
    slices: Vec<SviSlice>,
}

impl VolSurface {
    /// Build a surface from slices. Slices are sorted by expiry and validated
    /// for butterfly arbitrage and calendar (total-variance) monotonicity.
    /// Invalid slices are dropped; if that leaves nothing the surface is empty.
    pub fn new(mut slices: Vec<SviSlice>) -> Self {
        slices.retain(|s| s.t > 0.0 && s.params.is_butterfly_free());
        slices.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        // Enforce no calendar arbitrage: total variance must be non-decreasing
        // in expiry for every log-moneyness. Drop slices that violate it.
        let mut kept: Vec<SviSlice> = Vec::with_capacity(slices.len());
        for s in slices {
            if let Some(prev) = kept.last() {
                if !total_variance_monotone(prev, &s) {
                    continue;
                }
            }
            kept.push(s);
        }
        Self { slices: kept }
    }

    pub fn is_empty(&self) -> bool {
        self.slices.is_empty()
    }

    pub fn slices(&self) -> &[SviSlice] {
        &self.slices
    }

    /// Implied volatility at `strike` for spot `spot` and time to expiry
    /// `t_years`, interpolating slices in total variance.
    ///
    /// Extrapolation beyond the first/last expiry is flat in total variance
    /// per unit time (i.e. total variance scales linearly with `t`).
    pub fn vol(&self, strike: f64, spot: f64, t_years: f64) -> f64 {
        if self.slices.is_empty() || t_years <= 0.0 || spot <= 0.0 || strike <= 0.0 {
            return 0.0;
        }
        let k = (strike / spot).ln();
        let w = self.total_variance(k, t_years);
        if w <= 0.0 {
            0.0
        } else {
            (w / t_years).sqrt()
        }
    }

    /// Total implied variance at log-moneyness `k` and expiry `t_years`,
    /// interpolated linearly in `t` between slices and extrapolated flat in
    /// total variance per unit time outside the calibrated range.
    pub fn total_variance(&self, k: f64, t_years: f64) -> f64 {
        if self.slices.is_empty() || t_years <= 0.0 {
            return 0.0;
        }
        let first = &self.slices[0];
        if t_years <= first.t {
            // Flat total variance per unit time below the first slice.
            return first.params.total_variance(k) * (t_years / first.t);
        }
        let last = &self.slices[self.slices.len() - 1];
        if t_years >= last.t {
            return last.params.total_variance(k) * (t_years / last.t);
        }
        // Linear interpolation in total variance between bracketing slices.
        for pair in self.slices.windows(2) {
            let (lo, hi) = (&pair[0], &pair[1]);
            if t_years >= lo.t && t_years <= hi.t {
                let w_lo = lo.params.total_variance(k);
                let w_hi = hi.params.total_variance(k);
                let frac = (t_years - lo.t) / (hi.t - lo.t);
                return w_lo + frac * (w_hi - w_lo);
            }
        }
        last.params.total_variance(k) * (t_years / last.t)
    }

    /// Build a strike x expiry implied-volatility grid for the API.
    ///
    /// `strikes` and `expiries` are the axes; the returned grid is row-major
    /// with one row per expiry and one column per strike.
    pub fn iv_grid(&self, spot: f64, strikes: &[f64], expiries: &[f64]) -> Vec<Vec<f64>> {
        expiries
            .iter()
            .map(|&t| strikes.iter().map(|&k| self.vol(k, spot, t)).collect())
            .collect()
    }
}

/// Check that total variance does not decrease from slice `lo` to slice `hi`
/// for any log-moneyness (no calendar arbitrage).
fn total_variance_monotone(lo: &SviSlice, hi: &SviSlice) -> bool {
    if hi.t <= lo.t {
        return false;
    }
    let mut k = -5.0;
    while k <= 5.0 {
        let w_lo = lo.params.total_variance(k);
        let w_hi = hi.params.total_variance(k);
        if w_hi + 1e-9 < w_lo {
            return false;
        }
        k += 0.05;
    }
    true
}

/// Convert an implied Black-Scholes volatility to total variance.
pub fn iv_to_total_variance(iv: f64, t: f64) -> f64 {
    iv * iv * t
}

/// Convert total variance back to an implied Black-Scholes volatility.
pub fn total_variance_to_iv(w: f64, t: f64) -> f64 {
    if t <= 0.0 || w <= 0.0 {
        0.0
    } else {
        (w / t).sqrt()
    }
}

/// A single market observation used for calibration: a strike and its implied
/// volatility for a given expiry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quote {
    pub strike: f64,
    pub iv: f64,
}

/// Fit a raw SVI slice to a set of `(strike, iv)` quotes for expiry `t`.
///
/// Uses the Nelder-Mead simplex method (derivative-free, robust for the
/// non-convex SVI objective). Returns `None` if the fit does not converge to a
/// butterfly-free slice, so callers can keep their previous parameters.
pub fn calibrate_slice(spot: f64, t: f64, quotes: &[Quote]) -> Option<SviParams> {
    if quotes.len() < 5 || spot <= 0.0 || t <= 0.0 {
        return None;
    }
    // Initial guess: flat ATM vol, mild skew, small curvature.
    let atm_iv = quotes
        .iter()
        .map(|q| q.iv)
        .sum::<f64>()
        / quotes.len() as f64;
    let w_atm = atm_iv * atm_iv * t;
    let mut x = vec![w_atm, 0.1, -0.1, 0.0, 0.1];

    let objective = |p: &[f64]| -> f64 {
        let params = SviParams::new(p[0], p[1], p[2], p[3], p[4]);
        if !(params.b >= 0.0 && params.rho.abs() < 1.0 && params.sigma > 0.0) {
            return f64::INFINITY;
        }
        let mut err = 0.0;
        for q in quotes {
            let k = (q.strike / spot).ln();
            let model = params.implied_vol(k, t);
            let d = model - q.iv;
            err += d * d;
        }
        err
    };

    let fitted = nelder_mead(&mut x, &objective, 1e-10, 2000)?;
    let params = SviParams::new(fitted[0], fitted[1], fitted[2], fitted[3], fitted[4]);
    if params.is_butterfly_free() {
        Some(params)
    } else {
        None
    }
}

/// Minimise `f` starting from `x0` using the Nelder-Mead simplex method.
/// Returns the best point found, or `None` if the objective never became
/// finite (i.e. no valid parameters were explored).
fn nelder_mead<F: Fn(&[f64]) -> f64>(
    x0: &mut [f64],
    f: &F,
    tol: f64,
    max_iter: usize,
) -> Option<Vec<f64>> {
    let n = x0.len();
    let mut simplex: Vec<Vec<f64>> = Vec::with_capacity(n + 1);
    simplex.push(x0.to_vec());
    for i in 0..n {
        let mut p = x0.to_vec();
        let step = if p[i].abs() > 1e-6 { 0.05 * p[i].abs() } else { 0.05 };
        p[i] += step;
        simplex.push(p);
    }
    let mut values: Vec<f64> = simplex.iter().map(|p| f(p)).collect();

    let (alpha, gamma, rho, sigma) = (1.0, 2.0, 0.5, 0.5);
    for _ in 0..max_iter {
        // Order by objective value.
        let mut order: Vec<usize> = (0..simplex.len()).collect();
        order.sort_by(|&a, &b| values[a].partial_cmp(&values[b]).unwrap_or(std::cmp::Ordering::Equal));
        let sorted: Vec<Vec<f64>> = order.iter().map(|&i| simplex[i].clone()).collect();
        let sorted_vals: Vec<f64> = order.iter().map(|&i| values[i]).collect();
        simplex = sorted;
        values = sorted_vals;

        if !values[0].is_finite() {
            return None;
        }
        // Convergence: simplex is small in both value and geometry.
        let spread = (values[n] - values[0]).abs();
        let mut geom = 0.0;
        for i in 0..n {
            for j in 0..n {
                geom += (simplex[i + 1][j] - simplex[0][j]).abs();
            }
        }
        if spread < tol && geom < tol {
            break;
        }

        // Centroid of all but the worst point.
        let mut centroid = vec![0.0; n];
        for p in simplex.iter().take(n) {
            for j in 0..n {
                centroid[j] += p[j] / n as f64;
            }
        }

        // Reflection.
        let reflected: Vec<f64> = (0..n)
            .map(|j| centroid[j] + alpha * (centroid[j] - simplex[n][j]))
            .collect();
        let f_reflected = f(&reflected);

        if f_reflected < values[0] {
            // Expansion.
            let expanded: Vec<f64> = (0..n)
                .map(|j| centroid[j] + gamma * (reflected[j] - centroid[j]))
                .collect();
            let f_expanded = f(&expanded);
            if f_expanded < f_reflected {
                simplex[n] = expanded;
                values[n] = f_expanded;
            } else {
                simplex[n] = reflected;
                values[n] = f_reflected;
            }
        } else if f_reflected < values[n - 1] {
            simplex[n] = reflected;
            values[n] = f_reflected;
        } else {
            // Contraction.
            let contracted: Vec<f64> = if f_reflected < values[n] {
                (0..n)
                    .map(|j| centroid[j] + rho * (reflected[j] - centroid[j]))
                    .collect()
            } else {
                (0..n)
                    .map(|j| centroid[j] + rho * (simplex[n][j] - centroid[j]))
                    .collect()
            };
            let f_contracted = f(&contracted);
            if f_contracted < values[n] {
                simplex[n] = contracted;
                values[n] = f_contracted;
            } else {
                // Shrink toward the best point.
                for i in 1..=n {
                    for j in 0..n {
                        simplex[i][j] = simplex[0][j] + sigma * (simplex[i][j] - simplex[0][j]);
                    }
                    values[i] = f(&simplex[i]);
                }
            }
        }
    }

    if values[0].is_finite() {
        Some(simplex[0].clone())
    } else {
        None
    }
}

/// Convenience: build a surface from per-expiry quotes, calibrating each slice.
/// Slices that fail to calibrate are skipped.
pub fn calibrate_surface(spot: f64, slices: &[(f64, Vec<Quote>)]) -> VolSurface {
    let mut out = Vec::new();
    for (t, quotes) in slices {
        if let Some(params) = calibrate_slice(spot, *t, quotes) {
            out.push(SviSlice { t: *t, params });
        }
    }
    VolSurface::new(out)
}

/// The number of standard deviations used when generating a default grid.
pub const DEFAULT_GRID_STDDEVS: f64 = 3.0;

/// Build a default strike grid around `spot` for the surface API.
pub fn default_strike_grid(spot: f64, n: usize) -> Vec<f64> {
    if n == 0 || spot <= 0.0 {
        return Vec::new();
    }
    let lo = spot * (1.0 - 0.5 * DEFAULT_GRID_STDDEVS / 3.0);
    let hi = spot * (1.0 + 0.5 * DEFAULT_GRID_STDDEVS / 3.0);
    (0..n)
        .map(|i| lo + (hi - lo) * i as f64 / (n - 1).max(1) as f64)
        .collect()
}

/// A simple ATM-anchored flat slice, used as a fallback when no quotes exist.
pub fn flat_slice(t: f64, iv: f64) -> SviSlice {
    let w = iv * iv * t;
    SviSlice {
        t,
        params: SviParams::new(w, 0.0, 0.0, 0.0, 0.1),
    }
}

/// The number of calendar days in a year, used for expiry conversion.
pub const DAYS_PER_YEAR: f64 = 365.0;

/// Convert a number of days to years.
pub fn days_to_years(days: f64) -> f64 {
    days / DAYS_PER_YEAR
}

/// The number of radians in a full turn; kept for grid helpers.
pub const TWO_PI: f64 = 2.0 * PI;
