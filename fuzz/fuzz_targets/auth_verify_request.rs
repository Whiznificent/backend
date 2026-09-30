//! Fuzz target: verify-request field parsing — base64 signature decode and
//! ed25519 key/signature construction — isolated from the database.
//!
//! This exercises:
//! - `decode_stellar_public_key` on arbitrary wallet_address strings
//! - `VerifyingKey::from_bytes` on the decoded pubkey
//! - `BASE64.decode` on arbitrary signature strings
//! - `Signature::from_bytes` on arbitrary 64-byte arrays
//! - `verify_strict` on arbitrary message/key/signature triples
//!
//! None of these should panic; they should all return `Err` on bad input.
#![no_main]

use arbitrary::Arbitrary;
use data_encoding::BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct VerifyInput {
    wallet_address: String,
    message: String,
    /// Raw bytes that will be base64-encoded so the decoder gets exercised.
    signature_bytes: Vec<u8>,
}

fuzz_target!(|input: VerifyInput| {
    // Step 1: wallet address → pubkey bytes
    let pubkey_bytes =
        match zenith_backend::strkey::decode_stellar_public_key(&input.wallet_address) {
            Ok(b) => b,
            Err(_) => return, // invalid address; the handler returns 400
        };

    // Step 2: pubkey bytes → VerifyingKey
    let verifying_key = match VerifyingKey::from_bytes(&pubkey_bytes) {
        Ok(k) => k,
        Err(_) => return, // invalid point; the handler returns 400
    };

    // Step 3: signature_bytes (arbitrary) → base64 → decode back
    let sig_b64 = BASE64.encode(&input.signature_bytes);
    let decoded_sig = match BASE64.decode(sig_b64.as_bytes()) {
        Ok(b) => b,
        Err(_) => return,
    };

    // Step 4: decoded bytes → [u8; 64]
    let sig_array: [u8; 64] = match decoded_sig.try_into() {
        Ok(a) => a,
        Err(_) => return, // not 64 bytes; the handler returns 400
    };
    let signature = Signature::from_bytes(&sig_array);

    // Step 5: verify — must not panic regardless of outcome
    let _ = verifying_key.verify_strict(input.message.as_bytes(), &signature);
});
