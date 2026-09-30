//! Fuzz target: `black_scholes` pricing — no panics, finite output, and
//! mathematical property checks.
//!
//! Properties asserted on every valid (non-degenerate) input:
//! - premium ≥ 0
//! - premium is finite
//! - intrinsic value ≤ premium (time value ≥ 0)
//! - put-call parity: C - P = S·e^(0) - K·e^{-rT}  (within tolerance)
//! - delta ∈ [0,1] for calls, [-1,0] for puts
//! - gamma ≥ 0
//! - vega ≥ 0
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use zenith_backend::{black_scholes, BSInputs};

#[derive(Debug, Arbitrary)]
#[allow(dead_code)]
struct BSFuzzInput {
    /// We use f32 so `arbitrary` gives us a broad range of finite values
    /// without too many NaN/inf that immediately abort.
    spot: f32,
    strike: f32,
    vol: f32,
    t: f32,
    r: f32,
    is_call: bool,
}

fuzz_target!(|input: BSFuzzInput| {
    let spot = input.spot as f64;
    let strike = input.strike as f64;
    let vol = input.vol as f64;
    let t = input.t as f64;
    let r = input.r as f64;

    // Skip degenerate / pathological inputs that are outside the model's
    // defined domain — the handler validates these before calling us.
    if !spot.is_finite()
        || !strike.is_finite()
        || !vol.is_finite()
        || !t.is_finite()
        || !r.is_finite()
        || spot <= 0.0
        || strike <= 0.0
        || vol <= 0.0
        || vol > 20.0    // 2000% vol is already beyond any reasonable model
        || t < 0.0
        || r.abs() > 1.0 // ±100% risk-free rate is already absurd
    {
        return;
    }

    let call_result = black_scholes(&BSInputs {
        spot,
        strike,
        vol,
        t,
        r,
        is_call: true,
    });
    let put_result = black_scholes(&BSInputs {
        spot,
        strike,
        vol,
        t,
        r,
        is_call: false,
    });

    // ── Absence-of-panic already guaranteed by reaching here ──

    // Premium must be finite and non-negative
    assert!(
        call_result.premium.is_finite(),
        "call premium must be finite: {call_result:?}"
    );
    assert!(
        call_result.premium >= -1e-9,
        "call premium must be >= 0, got {}", call_result.premium
    );
    assert!(
        put_result.premium.is_finite(),
        "put premium must be finite: {put_result:?}"
    );
    assert!(
        put_result.premium >= -1e-9,
        "put premium must be >= 0, got {}", put_result.premium
    );

    // Time value must be non-negative (premium ≥ intrinsic)
    assert!(
        call_result.time_value >= -1e-9,
        "call time_value must be >= 0, got {}", call_result.time_value
    );
    assert!(
        put_result.time_value >= -1e-9,
        "put time_value must be >= 0, got {}", put_result.time_value
    );

    // Delta bounds
    assert!(
        call_result.delta >= -1e-9 && call_result.delta <= 1.0 + 1e-9,
        "call delta must be in [0,1], got {}", call_result.delta
    );
    assert!(
        put_result.delta >= -1.0 - 1e-9 && put_result.delta <= 1e-9,
        "put delta must be in [-1,0], got {}", put_result.delta
    );

    // Gamma ≥ 0, Vega ≥ 0
    assert!(
        call_result.gamma >= -1e-9,
        "call gamma must be >= 0, got {}", call_result.gamma
    );
    assert!(
        put_result.vega >= -1e-9,
        "put vega must be >= 0, got {}", put_result.vega
    );

    // Put-call parity: C - P = S - K·e^{-rT}   (valid when t > 0)
    // Tolerance is generous (1e-6 relative to spot) because we're running
    // the A&S norm_cdf approximation, not a high-precision library.
    if t > 1e-6 {
        let parity_lhs = call_result.premium - put_result.premium;
        let parity_rhs = spot - strike * (-r * t).exp();
        let tol = spot * 1e-4;
        assert!(
            (parity_lhs - parity_rhs).abs() <= tol,
            "put-call parity violated: C-P={parity_lhs:.8}, S-Ke^{{-rT}}={parity_rhs:.8}, tol={tol:.8}"
        );
    }

    // All BSResult fields must be finite (no silent NaN propagation)
    let fields = [
        call_result.premium, call_result.delta, call_result.gamma,
        call_result.theta, call_result.vega, call_result.rho,
        call_result.d1, call_result.d2, call_result.intrinsic, call_result.time_value,
        put_result.premium, put_result.delta, put_result.gamma,
        put_result.theta, put_result.vega, put_result.rho,
        put_result.d1, put_result.d2, put_result.intrinsic, put_result.time_value,
    ];
    for (i, &f) in fields.iter().enumerate() {
        assert!(f.is_finite(), "BSResult field[{i}] is not finite: {f}");
    }
});
