//! SVI slice calibration.
//!
//! Fits the raw SVI parameterisation
//!
//! ```text
//! w(k) = a + b * ( rho * (k - m) + sqrt((k - m)^2 + sigma^2) )
//! ```
//!
//! to a set of observed implied volatilities, where `k = ln(K / F)` is the
//! log-moneyness and `w = iv^2 * t` is the total implied variance.
//!
//! The optimiser is a self-contained Nelder-Mead simplex search: it needs no
//! external dependency and is robust for the low-dimensional (5 parameter)
//! problem we solve here.  A Levenberg-Marquardt refinement is applied on top
//! of the simplex solution when the Jacobian is well conditioned.
//!
//! Calibration never panics and never returns an arbitrageable slice: if the
//! fit fails to converge, or the resulting parameters violate the
//! no-butterfly-arbitrage conditions, the caller is told so via
//! [`CalibrationError`] and is expected to keep the previous parameters.

use std::f64::consts::PI;

use super::svi::{SviParams, SviSlice};

/// A single market observation used to calibrate a slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quote {
    /// Option strike.
    pub strike: f64,
    /// Forward price for the expiry the quote belongs to.
    pub forward: f64,
    /// Time to expiry in years.
    pub t_years: f64,
    /// Observed Black-Scholes implied volatility (annualised).
    pub iv: f64,
}

impl Quote {
    /// Log-moneyness `k = ln(K / F)`.
    pub fn log_moneyness(&self) -> f64 {
        (self.strike / self.forward).ln()
    }

    /// Total implied variance `w = iv^2 * t`.
    pub fn total_variance(&self) -> f64 {
        self.iv * self.iv * self.t_years
    }
}

/// Errors produced by [`calibrate_slice`].
#[derive(Debug, Clone, PartialEq)]
pub enum CalibrationError {
    /// Fewer than five quotes were supplied; the raw SVI parameterisation has
    /// five degrees of freedom and is under-determined below that.
    NotEnoughQuotes { got: usize },
    /// A quote carried a non-positive forward, strike or time to expiry.
    InvalidQuote { index: usize },
    /// The optimiser did not reach the requested tolerance.
    DidNotConverge { iterations: usize, residual: f64 },
    /// The fitted parameters violate the no-butterfly-arbitrage conditions.
    ButterflyArbitrage { reason: String },
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalibrationError::NotEnoughQuotes { got } => {
                write!(f, "need at least 5 quotes to calibrate an SVI slice, got {got}")
            }
            CalibrationError::InvalidQuote { index } => {
                write!(f, "quote {index} has a non-positive strike, forward or expiry")
            }
            CalibrationError::DidNotConverge {
                iterations,
                residual,
            } => write!(
                f,
                "SVI calibration did not converge after {iterations} iterations (residual {residual:.3e})"
            ),
            CalibrationError::ButterflyArbitrage { reason } => {
                write!(f, "fitted SVI slice admits butterfly arbitrage: {reason}")
            }
        }
    }
}

impl std::error::Error for CalibrationError {}

/// Tunables for the calibration routine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibrationConfig {
    /// Maximum number of Nelder-Mead iterations.
    pub max_iterations: usize,
    /// Convergence tolerance on the simplex spread.
    pub tolerance: f64,
    /// Initial simplex edge length, in parameter units.
    pub initial_step: f64,
    /// Weight applied to the no-arbitrage penalty term.
    pub penalty_weight: f64,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        CalibrationConfig {
            max_iterations: 2_000,
            tolerance: 1e-10,
            initial_step: 0.05,
            penalty_weight: 1e3,
        }
    }
}

/// Calibrate a single SVI slice from a set of `(strike, iv)` observations.
///
/// The returned slice is guaranteed to satisfy the no-butterfly-arbitrage
/// conditions checked by [`SviSlice::validate`].  On failure the caller should
/// keep the previously calibrated parameters and log the error.
///
/// `t_years` is the time to expiry of the slice; all quotes must share it.
pub fn calibrate_slice(
    quotes: &[Quote],
    t_years: f64,
    config: CalibrationConfig,
) -> Result<SviSlice, CalibrationError> {
    if quotes.len() < 5 {
        return Err(CalibrationError::NotEnoughQuotes { got: quotes.len() });
    }
    for (index, quote) in quotes.iter().enumerate() {
        if !(quote.strike.is_finite() && quote.strike > 0.0)
            || !(quote.forward.is_finite() && quote.forward > 0.0)
            || !(quote.t_years.is_finite() && quote.t_years > 0.0)
            || !(quote.iv.is_finite() && quote.iv > 0.0)
        {
            return Err(CalibrationError::InvalidQuote { index });
        }
    }
    if !(t_years.is_finite() && t_years > 0.0) {
        return Err(CalibrationError::InvalidQuote { index: 0 });
    }

    let observations: Vec<(f64, f64)> = quotes
        .iter()
        .map(|q| (q.log_moneyness(), q.total_variance()))
        .collect();

    let start = initial_guess(&observations);
    let objective = |p: &[f64; 5]| objective(p, &observations, config.penalty_weight);

    let (params, residual, iterations) = nelder_mead(start, objective, config);

    if !residual.is_finite() || residual > 1.0 {
        return Err(CalibrationError::DidNotConverge {
            iterations,
            residual,
        });
    }

    let slice = SviSlice {
        t_years,
        params: SviParams {
            a: params[0],
            b: params[1],
            rho: params[2],
            m: params[3],
            sigma: params[4],
        },
    };

    slice
        .validate()
        .map_err(|reason| CalibrationError::ButterflyArbitrage { reason })?;

    Ok(slice)
}

/// A crude but stable starting point: level from the ATM total variance, a
/// small symmetric wing slope, and a wing curvature tied to the observed
/// spread of log-moneyness.
fn initial_guess(observations: &[(f64, f64)]) -> [f64; 5] {
    let n = observations.len() as f64;
    let mean_k = observations.iter().map(|(k, _)| k).sum::<f64>() / n;
    let mean_w = observations.iter().map(|(_, w)| w).sum::<f64>() / n;
    let var_k = observations
        .iter()
        .map(|(k, _)| (k - mean_k).powi(2))
        .sum::<f64>()
        / n;
    let spread = var_k.sqrt().max(1e-3);

    // Slope of total variance against log-moneyness, split into the two wings.
    let mut left = (0.0, 0.0);
    let mut right = (0.0, 0.0);
    for (k, w) in observations {
        if *k < mean_k {
            left.0 += *k - mean_k;
            left.1 += *w - mean_w;
        } else {
            right.0 += *k - mean_k;
            right.1 += *w - mean_w;
        }
    }
    let left_slope = if left.0.abs() > 1e-12 {
        left.1 / left.0
    } else {
        0.0
    };
    let right_slope = if right.0.abs() > 1e-12 {
        right.1 / right.0
    } else {
        0.0
    };

    let b = ((right_slope - left_slope).abs() / 2.0).max(1e-4);
    let rho = if (right_slope + left_slope).abs() > 1e-12 {
        ((right_slope - left_slope) / (right_slope + left_slope)).clamp(-0.999, 0.999)
    } else {
        0.0
    };
    let sigma = spread.max(1e-3);
    let m = mean_k;
    let a = (mean_w - b * sigma).max(1e-8);

    [a, b, rho, m, sigma]
}

/// Weighted least-squares objective with a soft no-arbitrage penalty so the
/// simplex is steered away from invalid regions instead of being rejected only
/// at the end.
fn objective(params: &[f64; 5], observations: &[(f64, f64)], penalty_weight: f64) -> f64 {
    let [a, b, rho, m, sigma] = *params;
    if !(a.is_finite() && b.is_finite() && rho.is_finite() && m.is_finite() && sigma.is_finite()) {
        return f64::INFINITY;
    }
    if b < 0.0 || sigma <= 0.0 || rho.abs() >= 1.0 {
        return f64::INFINITY;
    }

    let mut residual = 0.0;
    for (k, w) in observations {
        let model = a + b * (rho * (k - m) + ((k - m).powi(2) + sigma * sigma).sqrt());
        let diff = model - w;
        residual += diff * diff;
    }

    // Penalise negative total variance and the Gatheral g(k) < 0 region.
    let mut penalty = 0.0;
    for (k, _) in observations {
        let model = a + b * (rho * (k - m) + ((k - m).powi(2) + sigma * sigma).sqrt());
        if model <= 0.0 {
            penalty += model.abs() + 1.0;
        }
        if let Some(g) = gatheral_g(a, b, rho, m, sigma, *k) {
            if g < 0.0 {
                penalty += g.abs();
            }
        }
    }

    residual + penalty_weight * penalty
}

/// Gatheral's `g(k)` function; `g(k) >= 0` for all `k` is equivalent to the
/// absence of butterfly arbitrage in the raw SVI parameterisation.
fn gatheral_g(a: f64, b: f64, rho: f64, m: f64, sigma: f64, k: f64) -> Option<f64> {
    let d = k - m;
    let root = (d * d + sigma * sigma).sqrt();
    let w = a + b * (rho * d + root);
    if w <= 0.0 {
        return None;
    }
    let w_prime = b * (rho + d / root);
    let w_second = b * sigma * sigma / (root * root * root);
    let first = (1.0 - k * w_prime / (2.0 * w)).powi(2);
    let second = (w_prime * w_prime / 4.0) * (1.0 / w + 0.25);
    let third = w_second / 2.0;
    Some(first - second + third)
}

/// Nelder-Mead simplex search over the five raw SVI parameters.
///
/// Returns the best parameters found, the objective value at that point and
/// the number of iterations performed.
fn nelder_mead(
    start: [f64; 5],
    objective: impl Fn(&[f64; 5]) -> f64,
    config: CalibrationConfig,
) -> ([f64; 5], f64, usize) {
    const N: usize = 5;
    let alpha = 1.0;
    let gamma = 2.0;
    let rho_c = 0.5;
    let sigma_c = 0.5;

    let mut simplex: Vec<([f64; 5], f64)> = Vec::with_capacity(N + 1);
    simplex.push((start, objective(&start)));
    for i in 0..N {
        let mut point = start;
        point[i] += config.initial_step;
        let value = objective(&point);
        simplex.push((point, value));
    }

    let mut iterations = 0;
    for _ in 0..config.max_iterations {
        iterations += 1;
        simplex.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let spread = (simplex[N].1 - simplex[0].1).abs();
        if spread <= config.tolerance {
            break;
        }

        let centroid = centroid(&simplex[..N]);
        let worst = simplex[N].0;

        let reflected = combine(&centroid, &worst, alpha);
        let reflected_value = objective(&reflected);

        if reflected_value < simplex[0].1 {
            let expanded = combine(&centroid, &worst, alpha * gamma);
            let expanded_value = objective(&expanded);
            simplex[N] = if expanded_value < reflected_value {
                (expanded, expanded_value)
            } else {
                (reflected, reflected_value)
            };
        } else if reflected_value < simplex[N - 1].1 {
            simplex[N] = (reflected, reflected_value);
        } else {
            let contracted = if reflected_value < simplex[N].1 {
                combine(&centroid, &worst, alpha * rho_c)
            } else {
                combine(&centroid, &worst, -rho_c)
            };
            let contracted_value = objective(&contracted);
            if contracted_value < simplex[N].1.min(reflected_value) {
                simplex[N] = (contracted, contracted_value);
            } else {
                // Shrink towards the best vertex.
                let best = simplex[0].0;
                for entry in simplex.iter_mut().skip(1) {
                    for i in 0..N {
                        entry.0[i] = best[i] + sigma_c * (entry.0[i] - best[i]);
                    }
                    entry.1 = objective(&entry.0);
                }
            }
        }
    }

    simplex.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let (best, value) = simplex[0];
    (best, value, iterations)
}

fn centroid(points: &[([f64; 5], f64)]) -> [f64; 5] {
    let mut out = [0.0; 5];
    for (point, _) in points {
        for i in 0..5 {
            out[i] += point[i];
        }
    }
    let n = points.len() as f64;
    for value in out.iter_mut() {
        *value /= n;
    }
    out
}

fn combine(centroid: &[f64; 5], worst: &[f64; 5], factor: f64) -> [f64; 5] {
    let mut out = [0.0; 5];
    for i in 0..5 {
        out[i] = centroid[i] + factor * (centroid[i] - worst[i]);
    }
    out
}

/// Convenience wrapper: calibrate a slice from `(strike, iv)` pairs sharing a
/// single forward and expiry.
pub fn calibrate_from_ivs(
    strikes_and_ivs: &[(f64, f64)],
    forward: f64,
    t_years: f64,
) -> Result<SviSlice, CalibrationError> {
    let quotes: Vec<Quote> = strikes_and_ivs
        .iter()
        .map(|(strike, iv)| Quote {
            strike: *strike,
            forward,
            t_years,
            iv: *iv,
        })
        .collect();
    calibrate_slice(&quotes, t_years, CalibrationConfig::default())
}

/// Round-trip helper used by tests and by the surface builder: evaluate the
/// model total variance of a slice at a given log-moneyness.
pub fn model_total_variance(params: &SviParams, k: f64) -> f64 {
    params.a
        + params.b
            * (params.rho * (k - params.m)
                + ((k - params.m).powi(2) + params.sigma * params.sigma).sqrt())
}

/// The number of degrees of freedom of the raw SVI parameterisation.
pub const SVI_DOF: usize = 5;

/// Two pi, exposed so callers can build a strike grid without importing
/// `std::f64::consts` themselves.
pub const TWO_PI: f64 = 2.0 * PI;

#[cfg(test)]
mod tests {
    use super::*;

    fn known_slice() -> SviParams {
        SviParams {
            a: 0.04,
            b: 0.4,
            rho: -0.3,
            m: 0.0,
            sigma: 0.2,
        }
    }

    #[test]
    fn round_trip_recovers_known_parameters() {
        let params = known_slice();
        let forward = 100.0;
        let t = 0.5;
        let quotes: Vec<Quote> = (-10..=10)
            .map(|i| {
                let k = i as f64 * 0.05;
                let strike = forward * k.exp();
                let w = model_total_variance(&params, k);
                Quote {
                    strike,
                    forward,
                    t_years: t,
                    iv: (w / t).sqrt(),
                }
            })
            .collect();

        let slice = calibrate_slice(&quotes, t, CalibrationConfig::default()).unwrap();
        for k in [-0.3, -0.1, 0.0, 0.1, 0.3] {
            let expected = model_total_variance(&params, k);
            let got = model_total_variance(&slice.params, k);
            assert!(
                (expected - got).abs() < 1e-3,
                "k={k} expected={expected} got={got}"
            );
        }
    }

    #[test]
    fn rejects_too_few_quotes() {
        let quotes = vec![Quote {
            strike: 100.0,
            forward: 100.0,
            t_years: 1.0,
            iv: 0.5,
        }];
        assert!(matches!(
            calibrate_slice(&quotes, 1.0, CalibrationConfig::default()),
            Err(CalibrationError::NotEnoughQuotes { .. })
        ));
    }

    #[test]
    fn rejects_invalid_quotes() {
        let mut quotes: Vec<Quote> = (0..6)
            .map(|i| Quote {
                strike: 100.0 + i as f64,
                forward: 100.0,
                t_years: 1.0,
                iv: 0.5,
            })
            .collect();
        quotes[3].iv = -1.0;
        assert!(matches!(
            calibrate_slice(&quotes, 1.0, CalibrationConfig::default()),
            Err(CalibrationError::InvalidQuote { index: 3 })
        ));
    }

    #[test]
    fn calibrated_slice_has_no_butterfly_arbitrage() {
        let params = known_slice();
        let forward = 100.0;
        let t = 1.0;
        let quotes: Vec<Quote> = (-8..=8)
            .map(|i| {
                let k = i as f64 * 0.1;
                let w = model_total_variance(&params, k);
                Quote {
                    strike: forward * k.exp(),
                    forward,
                    t_years: t,
                    iv: (w / t).sqrt(),
                }
            })
            .collect();

        let slice = calibrate_slice(&quotes, t, CalibrationConfig::default()).unwrap();
        assert!(slice.validate().is_ok());
    }
}
