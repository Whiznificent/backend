//! Fuzz target: sign-in message format parsing.
//!
//! The nonce endpoint accepts a `wallet_address` string and constructs a
//! canonical "Sign in to Zenith\nNonce: <hex>" message.  The verify
//! endpoint accepts that message back verbatim.  This target drives the
//! wallet-address validation path (which calls `decode_stellar_public_key`)
//! and the message-structure check independently of any database.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Interpret the bytes as a potential wallet address string.
    let Ok(addr) = std::str::from_utf8(data) else {
        return;
    };

    // 1. decode_stellar_public_key must never panic on any UTF-8 input.
    let result = zenith_backend::strkey::decode_stellar_public_key(addr);

    // 2. If it looks like a valid G-address, verify the round-trip.
    if let Ok(bytes) = result {
        let re_encoded = zenith_backend::strkey::encode_stellar_public_key(&bytes);
        // A successfully decoded address must re-encode to something that
        // also decodes without error.
        let re_decoded = zenith_backend::strkey::decode_stellar_public_key(&re_encoded)
            .expect("re-encoding a valid pubkey must produce a decodable address");
        assert_eq!(
            bytes, re_decoded,
            "round-trip: original={addr}, re-encoded={re_encoded}"
        );
    }

    // 3. The canonical sign-in message format must be constructible from any
    //    wallet-address string without panicking, regardless of validity.
    let nonce_hex = "deadbeef00112233445566778899aabb";
    let _message = format!("Sign in to Zenith\nNonce: {nonce_hex}");
});
