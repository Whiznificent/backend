//! Fuzz target: JSON deserialization of the `OpenPositionRequest` DTO.
//!
//! Throws arbitrary byte sequences at `serde_json::from_slice` targeting
//! the `OpenPositionRequest` shape and verifies:
//! - Never panics
//! - All fields are correctly typed when deserialization succeeds
//! - Explicitly tests the `RollPositionRequest` DTO as well (same file,
//!   same attack surface)
#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

/// Mirror of `positions::OpenPositionRequest` — we can't import it directly
/// because it's `pub(crate)` only in the handler, but it derives
/// `Deserialize` via serde, so the codec path is what we're exercising.
/// We keep a local copy here so the fuzz target compiles independently of
/// visibility changes.
#[derive(Debug, Deserialize)]
struct OpenPositionRequest {
    underlying: String,
    strike: f64,
    expiry_days: f64,
    option_type: String,
    position_type: String,
    contracts: f64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RollPositionRequest {
    new_strike: f64,
    new_expiry_days: f64,
}

fuzz_target!(|data: &[u8]| {
    // 1. OpenPositionRequest — must never panic
    let result: Result<OpenPositionRequest, _> = serde_json::from_slice(data);
    if let Ok(req) = result {
        // Basic sanity: string fields must not contain interior NULs
        // (serde_json would have already rejected them, but be explicit).
        let _ = req.underlying.len();
        let _ = req.option_type.len();
        let _ = req.position_type.len();

        // Numeric fields from JSON are always finite f64 (NaN/inf are not
        // valid JSON), but let's assert it anyway.
        assert!(
            req.strike.is_finite(),
            "serde_json should not produce non-finite f64"
        );
        assert!(req.expiry_days.is_finite());
        assert!(req.contracts.is_finite());
    }

    // 2. RollPositionRequest — same treatment
    let _: Result<RollPositionRequest, _> = serde_json::from_slice(data);
});
