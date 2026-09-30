//! Rotatable HMAC key rings for cursors, idempotency keys and webhook
//! signatures (issue #120).
//!
//! A ring holds the *current* key and, during a rotation window, the
//! *previous* one. New signatures always use the current key; verification
//! accepts either, so a rotation never invalidates tokens/records that were
//! signed before it. Once the longest-lived artifact signed by the previous
//! key has expired, the previous key is dropped from configuration and the
//! rotation completes (see `docs/secrets-rotation.md`).

use std::sync::Arc;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::secrets::{ExposeSecret, SecretString};

type HmacSha256 = Hmac<Sha256>;

/// Constant-time comparison is delegated to `hmac`'s `verify_slice`.
pub struct HmacKeyRing {
    current: Arc<SecretString>,
    previous: Option<Arc<SecretString>>,
}

impl HmacKeyRing {
    pub fn new(current: SecretString, previous: Option<SecretString>) -> Self {
        Self {
            current: Arc::new(current),
            previous: previous.map(Arc::new),
        }
    }

    /// Pull `{name}` (current) and, if present, `{name}_PREVIOUS` out of a
    /// secret store. The previous key is optional so a ring is usable before
    /// the first rotation and after one has completed.
    pub async fn from_store(
        store: &crate::secrets::SecretStore,
        name: &str,
        previous_name: &str,
    ) -> Result<Self, crate::secrets::SecretError> {
        let current = store.get(name).await?;
        let previous = match store.get(previous_name).await {
            Ok(value) => Some(value),
            Err(crate::secrets::SecretError::NotFound(_)) => None,
            Err(other) => return Err(other),
        };
        Ok(Self { current, previous })
    }

    pub fn has_previous(&self) -> bool {
        self.previous.is_some()
    }

    /// Sign `message` with the current key, returning a lowercase-hex tag.
    pub fn sign(&self, message: &[u8]) -> String {
        let tag = compute(self.current.expose_secret().as_bytes(), message);
        data_encoding::HEXLOWER.encode(tag.as_slice())
    }

    /// Verify a hex `tag` over `message` against the current key, then the
    /// previous one. Returns false for malformed hex, wrong tags, and any
    /// other non-match — callers must not distinguish those cases.
    pub fn verify(&self, message: &[u8], tag_hex: &str) -> bool {
        let Ok(tag) = data_encoding::HEXLOWER_PERMISSIVE.decode(tag_hex.as_bytes()) else {
            return false;
        };
        if verify_with(self.current.expose_secret().as_bytes(), message, &tag) {
            return true;
        }
        self.previous
            .as_ref()
            .is_some_and(|key| verify_with(key.expose_secret().as_bytes(), message, &tag))
    }
}

fn compute(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn verify_with(key: &[u8], message: &[u8], tag: &[u8]) -> bool {
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return false;
    };
    mac.update(message);
    mac.verify_slice(tag).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(current: &str, previous: Option<&str>) -> HmacKeyRing {
        HmacKeyRing::new(
            SecretString::from(current.to_string()),
            previous.map(|key| SecretString::from(key.to_string())),
        )
    }

    #[test]
    fn signing_is_deterministic_and_keyed() {
        let first = ring("key-a", None).sign(b"payload");
        let second = ring("key-a", None).sign(b"payload");
        let other = ring("key-b", None).sign(b"payload");
        assert_eq!(first, second);
        assert_ne!(first, other);
    }

    #[test]
    fn verifies_signatures_from_the_current_key() {
        let ring = ring("key-a", None);
        let tag = ring.sign(b"payload");
        assert!(ring.verify(b"payload", &tag));
    }

    #[test]
    fn verifies_signatures_from_the_previous_key_during_rotation() {
        // Signed before the rotation, verified after it, while the old key is
        // still configured as `previous`.
        let old_tag = ring("old-key", None).sign(b"cursor-42");
        let rotated = ring("new-key", Some("old-key"));
        assert!(rotated.has_previous());
        assert!(rotated.verify(b"cursor-42", &old_tag));
    }

    #[test]
    fn a_completed_rotation_rejects_the_dropped_key() {
        let old_tag = ring("old-key", None).sign(b"cursor-42");
        let completed = ring("new-key", None);
        assert!(!completed.verify(b"cursor-42", &old_tag));
    }

    #[test]
    fn rejects_tampered_messages_and_garbage_tags() {
        let ring = ring("key-a", Some("key-b"));
        let tag = ring.sign(b"payload");
        assert!(!ring.verify(b"payload!", &tag));
        assert!(!ring.verify(b"payload", "not-hex"));
        assert!(!ring.verify(b"payload", "00ff"));
    }

    #[test]
    fn previous_key_is_optional() {
        assert!(!ring("key-a", None).has_previous());
    }
}
