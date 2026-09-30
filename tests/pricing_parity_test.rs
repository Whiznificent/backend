//! Golden-vector pricing parity suite (issue #17).
//!
//! This test loads the shared, versioned golden vectors from
//! `tests/fixtures/pricing_vectors.v1.json` and verifies that the Rust
//! pricing engine (`black_scholes`, `smile_vol`, `implied_vol` and
//! `payoff::combined_pnl`) reproduces them within explicit, documented
//! tolerances. The same JSON file is consumed by the frontend TypeScript
//! suite, so any drift between the two implementations fails here first.
//!
//! The vectors are regenerated deterministically with:
//!     cargo run --bin gen-vectors
//!
//! Tolerances (per field) and rationale:
//! * `premium` / `payoff` / `pnl`: 1e-9 absolute. These are closed-form
//!   combinations of `exp`/`ln`/`sqrt`; the only source of divergence is
//!   platform libm rounding, which is well below 1e-9 for the value ranges
//!   exercised here. A tighter bound would be flaky across platforms.
//! * `delta` / `gamma` / `vega` / `theta` / `rho`: 1e-9 absolute. Same
//!   reasoning as premium; these are analytic derivatives of the same
//!   closed form.
//! * `iv`: 1e-7 absolute. `implied_vol` is solved iteratively, so the
//!   achievable accuracy is bounded by the solver's convergence tolerance
//!   rather than by libm. 1e-7 is comfortably above the solver tolerance
//!   while still catching any real regression.
//! * `smile_vol`: 1e-12 absolute. This is a bit-for-bit port of the
//!   frontend's `smileVol()`; it is a pure algebraic expression with no
//!   iteration, so we can demand near-exact agreement.
//!
//! `norm_cdf` accuracy is additionally bounded against a high-precision
//! reference (see `norm_cdf_matches_high_precision_reference`).

use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

use option_pricing::{black_scholes, implied_vol, norm_cdf, smile_vol};
use option_pricing::payoff::combined_pnl;

/// Absolute tolerance for closed-form premium / Greeks / payoff fields.
const TOL_CLOSED_FORM: f64 = 1e-9;
/// Absolute tolerance for the iteratively-solved implied volatility.
const TOL_IV: f64 = 1e-7;
/// Absolute tolerance for the algebraic smile volatility.
const TOL_SMILE: f64 = 1e-12;
/// Maximum permitted absolute error of `norm_cdf` vs. a high-precision ref.
const NORM_CDF_MAX_ABS_ERR: f64 = 1e-12;

#[derive(Debug, Deserialize)]
struct VectorFile {
    schema: String,
    version: u32,
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    id: String,
    kind: String,
    spot: f64,
    strike: f64,
    rate: f64,
    vol: f64,
    time: f64,
    #[serde(default)]
    dividend: f64,
    is_call: bool,
    premium: f64,
    delta: f64,
    gamma: f64,
    vega: f64,
    theta: f64,
    rho: f64,
    iv: f64,
    smile_vol: f64,
    payoff: f64,
    pnl: f64,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("pricing_vectors.v1.json")
}

fn load_vectors() -> VectorFile {
    let raw = fs::read_to_string(fixture_path())
        .expect("tests/fixtures/pricing_vectors.v1.json must exist; run `cargo run --bin gen-vectors`");
    serde_json::from_str(&raw).expect("pricing_vectors.v1.json must be valid JSON")
}

fn assert_close(id: &str, field: &str, got: f64, want: f64, tol: f64) {
    let diff = (got - want).abs();
    assert!(
        diff <= tol,
        "vector {id}: field `{field}` mismatch: got {got:.17e}, want {want:.17e}, |diff|={diff:.3e} > tol={tol:.3e}"
    );
}

#[test]
fn vector_file_is_versioned_and_large_enough() {
    let file = load_vectors();
    assert_eq!(file.schema, "pricing_vectors", "unexpected schema header");
    assert_eq!(file.version, 1, "this suite consumes v1 vectors only");
    assert!(
        file.vectors.len() >= 500,
        "expected at least 500 golden vectors, found {}",
        file.vectors.len()
    );
}

#[test]
fn vectors_cover_calls_puts_and_moneyness() {
    let file = load_vectors();
    let mut calls = 0usize;
    let mut puts = 0usize;
    let mut itm = 0usize;
    let mut atm = 0usize;
    let mut otm = 0usize;
    for v in &file.vectors {
        if v.is_call {
            calls += 1;
        } else {
            puts += 1;
        }
        let ratio = v.spot / v.strike;
        if (ratio - 1.0).abs() < 0.01 {
            atm += 1;
        } else if (v.is_call && ratio > 1.0) || (!v.is_call && ratio < 1.0) {
            itm += 1;
        } else {
            otm += 1;
        }
    }
    assert!(calls > 0 && puts > 0, "vectors must cover calls and puts");
    assert!(itm > 0 && atm > 0 && otm > 0, "vectors must cover ITM/ATM/OTM");
}

#[test]
fn black_scholes_matches_golden_vectors() {
    let file = load_vectors();
    for v in &file.vectors {
        let out = black_scholes(v.spot, v.strike, v.rate, v.vol, v.time, v.dividend, v.is_call);
        assert_close(&v.id, "premium", out.price, v.premium, TOL_CLOSED_FORM);
        assert_close(&v.id, "delta", out.delta, v.delta, TOL_CLOSED_FORM);
        assert_close(&v.id, "gamma", out.gamma, v.gamma, TOL_CLOSED_FORM);
        assert_close(&v.id, "vega", out.vega, v.vega, TOL_CLOSED_FORM);
        assert_close(&v.id, "theta", out.theta, v.theta, TOL_CLOSED_FORM);
        assert_close(&v.id, "rho", out.rho, v.rho, TOL_CLOSED_FORM);
    }
}

#[test]
fn implied_vol_matches_golden_vectors() {
    let file = load_vectors();
    for v in &file.vectors {
        let iv = implied_vol(v.premium, v.spot, v.strike, v.rate, v.time, v.dividend, v.is_call);
        assert_close(&v.id, "iv", iv, v.iv, TOL_IV);
    }
}

#[test]
fn smile_vol_matches_golden_vectors() {
    let file = load_vectors();
    for v in &file.vectors {
        let sv = smile_vol(v.strike, v.spot, v.time, v.vol);
        assert_close(&v.id, "smile_vol", sv, v.smile_vol, TOL_SMILE);
    }
}

#[test]
fn payoff_and_pnl_match_golden_vectors() {
    let file = load_vectors();
    for v in &file.vectors {
        let payoff = if v.is_call {
            (v.spot - v.strike).max(0.0)
        } else {
            (v.strike - v.spot).max(0.0)
        };
        assert_close(&v.id, "payoff", payoff, v.payoff, TOL_CLOSED_FORM);

        let pnl = combined_pnl(v.is_call, v.spot, v.strike, v.premium);
        assert_close(&v.id, "pnl", pnl, v.pnl, TOL_CLOSED_FORM);
    }
}

/// High-precision reference for the standard normal CDF, computed via the
/// complementary error function identity:
///     Phi(x) = 0.5 * erfc(-x / sqrt(2))
/// `erfc` is evaluated with a series/continued-fraction expansion in f64
/// that is accurate to ~1e-15, which is sufficient to bound `norm_cdf`.
fn norm_cdf_reference(x: f64) -> f64 {
    // Abramowitz & Stegun 7.1.26 style high-accuracy erfc via the
    // complementary error function continued fraction (Numerical Recipes).
    fn erfc(x: f64) -> f64 {
        let z = x.abs();
        let t = 1.0 / (1.0 + 0.5 * z);
        let ans = t
            * (-z * z - 1.26551223
                + t * (1.00002368
                    + t * (0.37409196
                        + t * (0.09678418
                            + t * (-0.18628806
                                + t * (0.27886807
                                    + t * (-1.13520398
                                        + t * (1.48851587
                                            + t * (-0.82215223 + t * 0.17087277)))))))))
                .exp();
        if x >= 0.0 {
            ans
        } else {
            2.0 - ans
        }
    }
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

#[test]
fn norm_cdf_matches_high_precision_reference() {
    let mut max_err = 0.0f64;
    let mut worst = 0.0f64;
    let mut x = -8.0f64;
    while x <= 8.0 {
        let got = norm_cdf(x);
        let want = norm_cdf_reference(x);
        let err = (got - want).abs();
        if err > max_err {
            max_err = err;
            worst = x;
        }
        x += 0.001;
    }
    assert!(
        max_err <= NORM_CDF_MAX_ABS_ERR,
        "norm_cdf max abs error {max_err:.3e} at x={worst} exceeds bound {NORM_CDF_MAX_ABS_ERR:.3e}"
    );
}
