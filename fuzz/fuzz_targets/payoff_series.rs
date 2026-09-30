//! Fuzz target: `combined_pnl` and `combined_payoff_series`.
//!
//! Properties asserted:
//! - Never panics
//! - `combined_payoff_series` returns exactly `steps + 1` points
//! - Every point's `pnl` equals the direct `combined_pnl` call at that spot
//! - `net_premium` of a long+short pair with same contracts/premium is ~0
//! - All output values are finite
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use zenith_backend::payoff::{combined_payoff_series, combined_pnl, net_premium, PricedLeg};

#[derive(Debug, Arbitrary)]
struct LegInput {
    option_type: bool, // false = call, true = put
    position_type: bool, // false = long, true = short
    strike: f32,
    contracts: f32,
    premium: f32,
}

impl From<LegInput> for PricedLeg {
    fn from(l: LegInput) -> Self {
        PricedLeg {
            option_type: if l.option_type { "put" } else { "call" }.to_string(),
            position_type: if l.position_type { "short" } else { "long" }.to_string(),
            strike: l.strike as f64,
            contracts: l.contracts as f64,
            premium: l.premium as f64,
        }
    }
}

#[derive(Debug, Arbitrary)]
struct PayoffFuzzInput {
    /// 1–8 legs (bounded to keep the fuzzer fast)
    legs: [LegInput; 4],
    lo_spot: f32,
    hi_spot: f32,
    /// 1–500 steps
    steps_raw: u16,
}

fuzz_target!(|input: PayoffFuzzInput| {
    let legs: Vec<PricedLeg> = input
        .legs
        .into_iter()
        .map(PricedLeg::from)
        .collect();

    // Must not panic even on garbage floats (NaN/inf strikes/premiums).
    let spot = input.lo_spot as f64;
    let _ = combined_pnl(&legs, spot);

    // Only proceed with the series checks for a valid (non-degenerate) range.
    let lo = input.lo_spot as f64;
    let hi = input.hi_spot as f64;
    let steps = (input.steps_raw as u32 % 500) + 1; // 1..=500

    if !lo.is_finite() || !hi.is_finite() || hi <= lo {
        return;
    }

    let series = combined_payoff_series(&legs, lo, hi, steps);

    // Series must have exactly steps+1 elements.
    assert_eq!(
        series.len(),
        steps as usize + 1,
        "combined_payoff_series returned wrong number of points"
    );

    // Each point must match the direct pnl call.
    for pt in &series {
        let direct = combined_pnl(&legs, pt.spot);
        assert!(
            (pt.pnl - direct).abs() < 1e-9,
            "series pnl mismatch: pt.pnl={} direct={direct} at spot={}", pt.pnl, pt.spot
        );
        // Values must be finite (no NaN/inf propagation from degenerate legs)
        // — but only for finite leg parameters, since we deliberately pass
        // garbage above.
        let all_finite = legs.iter().all(|l| {
            l.strike.is_finite() && l.contracts.is_finite() && l.premium.is_finite()
        });
        if all_finite {
            assert!(pt.pnl.is_finite(), "series pnl is not finite at spot={}", pt.spot);
        }
    }

    // net_premium: long + short same leg → net ≈ 0
    if !legs.is_empty() {
        let leg0 = &legs[0];
        if leg0.contracts.is_finite() && leg0.premium.is_finite() {
            let twin = PricedLeg {
                option_type: leg0.option_type.clone(),
                position_type: if leg0.position_type == "long" {
                    "short".to_string()
                } else {
                    "long".to_string()
                },
                strike: leg0.strike,
                contracts: leg0.contracts,
                premium: leg0.premium,
            };
            let pair = vec![leg0.clone(), twin];
            let net = net_premium(&pair);
            assert!(
                net.abs() < 1e-9,
                "long+short pair net_premium should be ~0, got {net}"
            );
        }
    }
});
