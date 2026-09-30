//! Fuzz target: `implied_vol` Newton-Raphson solver.
//!
//! Properties asserted:
//! - Must never panic (regardless of input)
//! - When `Some(iv)` is returned, `iv` must be finite and positive
//! - When given a price computed by `black_scholes` with known vol, the
//!   recovered IV must be within 1e-3 of the original (round-trip check)
//! - The solver must terminate (no infinite loops) — enforced by
//!   libFuzzer's `-timeout` flag in CI
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use zenith_backend::{black_scholes, implied_vol, BSInputs};

#[derive(Debug, Arbitrary)]
struct IVFuzzInput {
    market_price: f32,
    spot: f32,
    strike: f32,
    t: f32,
    r: f32,
    is_call: bool,
}

fuzz_target!(|input: IVFuzzInput| {
    let market_price = input.market_price as f64;
    let spot = input.spot as f64;
    let strike = input.strike as f64;
    let t = input.t as f64;
    let r = input.r as f64;

    // 1. Must not panic on any finite input.
    if market_price.is_finite()
        && spot.is_finite()
        && strike.is_finite()
        && t.is_finite()
        && r.is_finite()
    {
        let result = implied_vol(market_price, spot, strike, t, r, input.is_call);

        // 2. If Some, the returned vol must be finite and positive.
        if let Some(iv) = result {
            assert!(
                iv.is_finite(),
                "implied_vol returned non-finite Some({iv})"
            );
            assert!(
                iv > 0.0,
                "implied_vol returned non-positive Some({iv})"
            );
        }
    }

    // 3. Round-trip: price via BS → recover via IV → must match within 1e-3.
    //    Only attempt this for inputs in a domain where the solver converges
    //    reliably: moderate spot/strike ratio, realistic vol and time.
    if spot > 1.0
        && spot < 1_000_000.0
        && strike > 1.0
        && strike < 1_000_000.0
        && t > 1.0 / 365.0   // at least 1 day
        && t < 5.0            // at most 5 years
        && r.abs() < 0.5
    {
        // Use a known "reasonable" vol for the forward direction
        let true_vol = 0.5_f64; // 50 % — safely within solver convergence range
        let price = black_scholes(&BSInputs {
            spot,
            strike,
            vol: true_vol,
            t,
            r,
            is_call: input.is_call,
        })
        .premium;

        if price.is_finite() && price >= 0.0 {
            if let Some(recovered) = implied_vol(price, spot, strike, t, r, input.is_call) {
                assert!(
                    (recovered - true_vol).abs() < 1e-3,
                    "IV round-trip failed: true_vol={true_vol}, recovered={recovered}, \
                     spot={spot}, strike={strike}, t={t}, price={price}"
                );
            }
        }
    }
});
