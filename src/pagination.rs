//! Opaque, signed keyset (cursor) pagination primitives.
//!
//! A cursor encodes the `(sort_key, id)` tuple of the last row of a page,
//! base64url-encodes it, and HMAC-signs it so it cannot be tampered with.
//! The filter set that produced the page is bound into the signature so a
//! cursor minted for one filter set is rejected when replayed against another.
//!
//! See <https://use-the-index-luke.com/no-offset> for the rationale behind
//! keyset pagination over `LIMIT/OFFSET`.

use std::marker::PhantomData;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::Sha256;

use crate::error::ApiError;

/// Maximum number of rows a single list request may return.
pub const MAX_LIST_LIMIT: u32 = 100;

/// Default page size when the caller does not supply `limit`.
pub const DEFAULT_LIST_LIMIT: u32 = 25;

type HmacSha256 = Hmac<Sha256>;

/// The decoded payload carried by a cursor.
///
/// `sort_key` is the value of the ordering column (e.g. `opened_at`) and `id`
/// is the tiebreaker for rows that share the same `sort_key`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CursorPayload<K> {
    pub sort_key: K,
    pub id: i64,
}

/// A validated, opaque keyset cursor.
///
/// The inner payload is only produced by [`Cursor::decode`], which verifies the
/// HMAC signature and the bound filter fingerprint before returning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor<K> {
    payload: CursorPayload<K>,
}

impl<K> Cursor<K> {
    /// The `(sort_key, id)` tuple to resume the query from.
    pub fn payload(&self) -> &CursorPayload<K> {
        &self.payload
    }

    /// Consume the cursor, yielding the `(sort_key, id)` tuple.
    pub fn into_payload(self) -> CursorPayload<K> {
        self.payload
    }
}

impl<K> Cursor<K>
where
    K: Serialize + DeserializeOwned,
{
    /// Mint a signed cursor for the last row of a page.
    ///
    /// `filters` is a stable fingerprint of the active filter set; it is bound
    /// into the signature so a cursor cannot be replayed against a different
    /// filter set.
    pub fn encode(
        secret: &[u8],
        filters: &str,
        sort_key: K,
        id: i64,
    ) -> Result<String, ApiError> {
        let payload = CursorPayload { sort_key, id };
        let body = serde_json::to_vec(&payload)
            .map_err(|_| ApiError::internal("failed to encode cursor payload"))?;
        let encoded = URL_SAFE_NO_PAD.encode(&body);
        let signature = sign(secret, filters, &encoded)?;
        Ok(format!("{encoded}.{signature}"))
    }

    /// Decode and verify a cursor produced by [`Cursor::encode`].
    ///
    /// Returns an error when the cursor is malformed, the signature does not
    /// match, or the bound filter fingerprint differs from `filters`.
    pub fn decode(secret: &[u8], filters: &str, token: &str) -> Result<Self, ApiError> {
        let (encoded, signature) = token
            .split_once('.')
            .ok_or_else(|| ApiError::bad_request("malformed cursor"))?;

        let expected = sign(secret, filters, encoded)?;
        if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
            return Err(ApiError::bad_request("invalid cursor signature"));
        }

        let body = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| ApiError::bad_request("malformed cursor"))?;
        let payload: CursorPayload<K> = serde_json::from_slice(&body)
            .map_err(|_| ApiError::bad_request("malformed cursor"))?;

        Ok(Self { payload })
    }
}

/// Clamp a caller-supplied `limit` into `1..=MAX_LIST_LIMIT`.
pub fn normalize_limit(limit: Option<u32>) -> u32 {
    match limit {
        Some(0) | None => DEFAULT_LIST_LIMIT,
        Some(n) => n.min(MAX_LIST_LIMIT),
    }
}

/// Build a stable fingerprint of the active filter set.
///
/// The fingerprint is bound into the cursor signature so a cursor minted for
/// one filter set is rejected when replayed against another.
pub fn filter_fingerprint(parts: &[Option<&str>]) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            Some(value) => {
                out.push_str(value);
            }
            None => out.push('\u{0}'),
        }
        out.push('\u{1f}');
    }
    out
}

fn sign(secret: &[u8], filters: &str, encoded: &str) -> Result<String, ApiError> {
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| ApiError::internal("invalid cursor signing key"))?;
    mac.update(filters.as_bytes());
    mac.update(b"\u{1f}");
    mac.update(encoded.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Marker used by extractors that need to name the cursor's key type.
#[derive(Debug, Clone, Copy)]
pub struct CursorKey<K>(PhantomData<K>);

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret";

    #[test]
    fn round_trips_a_cursor() {
        let token = Cursor::<i64>::encode(SECRET, "filters", 1_700_000_000, 42).unwrap();
        let cursor = Cursor::<i64>::decode(SECRET, "filters", &token).unwrap();
        assert_eq!(cursor.payload().sort_key, 1_700_000_000);
        assert_eq!(cursor.payload().id, 42);
    }

    #[test]
    fn rejects_a_tampered_cursor() {
        let token = Cursor::<i64>::encode(SECRET, "filters", 1, 1).unwrap();
        let tampered = format!("{}x", token);
        assert!(Cursor::<i64>::decode(SECRET, "filters", &tampered).is_err());
    }

    #[test]
    fn rejects_a_cursor_from_a_different_filter_set() {
        let token = Cursor::<i64>::encode(SECRET, "status=open", 1, 1).unwrap();
        assert!(Cursor::<i64>::decode(SECRET, "status=closed", &token).is_err());
    }

    #[test]
    fn clamps_limits() {
        assert_eq!(normalize_limit(None), DEFAULT_LIST_LIMIT);
        assert_eq!(normalize_limit(Some(0)), DEFAULT_LIST_LIMIT);
        assert_eq!(normalize_limit(Some(10)), 10);
        assert_eq!(normalize_limit(Some(MAX_LIST_LIMIT + 1)), MAX_LIST_LIMIT);
    }
}
