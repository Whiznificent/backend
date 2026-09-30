//! Fuzz target: strkey decode/encode round-trip and decode-only paths.
//!
//! Exercises every branch in `decode_stellar_public_key`:
//! - base32 decoding errors
//! - wrong payload length
//! - wrong version byte
//! - checksum mismatch
//! - success (plus a round-trip check that encode → decode is identity)
#![no_main]

use libfuzzer_sys::fuzz_target;
use zenith_backend::strkey::{decode_stellar_public_key, encode_stellar_public_key};

fuzz_target!(|data: &[u8]| {
    // 1. Raw arbitrary bytes — must never panic.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = decode_stellar_public_key(s);
    }

    // 2. If we have exactly 32 bytes, test the full encode → decode round-trip.
    if data.len() == 32 {
        let arr: [u8; 32] = data.try_into().unwrap();
        let encoded = encode_stellar_public_key(&arr);

        // encode must always produce a 'G'-prefixed string
        assert!(
            encoded.starts_with('G'),
            "encode_stellar_public_key must produce a 'G' prefix, got: {encoded}"
        );

        // decode must recover the original key exactly
        let decoded = decode_stellar_public_key(&encoded)
            .expect("decode_stellar_public_key must succeed on output of encode_stellar_public_key");
        assert_eq!(
            decoded, arr,
            "round-trip failure: encoded={encoded}"
        );
    }
});
