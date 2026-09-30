//! Fuzz target: JSON deserialization of the `PayoffRequest` DTO.
//!
//! Exercises the full `legs` array deserialization path (variable-length
//! array of `PricedLeg` objects) and the optional `steps` field.
#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PricedLeg {
    option_type: String,
    position_type: String,
    strike: f64,
    contracts: f64,
    premium: f64,
}

#[derive(Debug, Deserialize)]
struct PayoffRequest {
    legs: Vec<PricedLeg>,
    lo_spot: f64,
    hi_spot: f64,
    #[serde(default)]
    steps: u32,
}

fuzz_target!(|data: &[u8]| {
    let result: Result<PayoffRequest, _> = serde_json::from_slice(data);
    if let Ok(req) = result {
        // All f64 fields coming from valid JSON must be finite.
        assert!(req.lo_spot.is_finite());
        assert!(req.hi_spot.is_finite());
        for leg in &req.legs {
            assert!(leg.strike.is_finite());
            assert!(leg.contracts.is_finite());
            assert!(leg.premium.is_finite());
        }

        // If the request passes basic validation, run the math directly to
        // ensure no panics even on extreme-but-valid JSON values.
        if !req.legs.is_empty()
            && req.hi_spot > req.lo_spot
            && req.steps > 0
            && req.steps <= 10_000
        {
            let legs: Vec<zenith_backend::payoff::PricedLeg> = req
                .legs
                .into_iter()
                .map(|l| zenith_backend::payoff::PricedLeg {
                    option_type: l.option_type,
                    position_type: l.position_type,
                    strike: l.strike,
                    contracts: l.contracts,
                    premium: l.premium,
                })
                .collect();
            let _ = zenith_backend::payoff::combined_payoff_series(
                &legs,
                req.lo_spot,
                req.hi_spot,
                req.steps,
            );
        }
    }
});
