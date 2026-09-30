//! Fuzz target: JSON deserialization of Alert and Watchlist request DTOs.
//!
//! Covers:
//! - `CreateAlertRequest` (underlying, condition, target_price)
//! - `AddWatchlistRequest` (underlying)
//! - `NonceRequest` (wallet_address)
//! - `VerifyRequest` (wallet_address, message, signature)
//!
//! All must deserialize without panicking; further validation happens at
//! the handler level (not tested here — we just want the serde codec to
//! be panic-free).
#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CreateAlertRequest {
    underlying: String,
    condition: String, // "above" | "below"
    target_price: f64,
}

#[derive(Debug, Deserialize)]
struct AddWatchlistRequest {
    underlying: String,
}

#[derive(Debug, Deserialize)]
struct NonceRequest {
    wallet_address: String,
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    wallet_address: String,
    message: String,
    signature: String,
}

fuzz_target!(|data: &[u8]| {
    // Alert creation
    if let Ok(req) = serde_json::from_slice::<CreateAlertRequest>(data) {
        assert!(req.target_price.is_finite());
        let _ = req.underlying.len();
        let _ = req.condition.len();
    }

    // Watchlist add
    if let Ok(req) = serde_json::from_slice::<AddWatchlistRequest>(data) {
        let _ = req.underlying.len();
    }

    // Auth nonce
    if let Ok(req) = serde_json::from_slice::<NonceRequest>(data) {
        let _ = req.wallet_address.len();
    }

    // Auth verify
    if let Ok(req) = serde_json::from_slice::<VerifyRequest>(data) {
        let _ = req.wallet_address.len();
        let _ = req.message.len();
        let _ = req.signature.len();
    }
});
