use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct SponsorKeyPool {
    keys: Vec<SigningKey>,
    current_index: AtomicUsize,
}

impl SponsorKeyPool {
    pub fn new(keys: Vec<SigningKey>) -> Self {
        Self {
            keys,
            current_index: AtomicUsize::new(0),
        }
    }

    pub fn random_single() -> Self {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let key = SigningKey::from_bytes(&seed);
        Self::new(vec![key])
    }

    pub fn next_sponsor(&self) -> (&SigningKey, String) {
        let idx = self.current_index.fetch_add(1, Ordering::Relaxed) % self.keys.len();
        let key = &self.keys[idx];
        let address = crate::strkey::encode_stellar_public_key(key.verifying_key().as_bytes());
        (key, address)
    }

    pub fn sign_payload(&self, payload: &[u8]) -> (String, Vec<u8>) {
        let (key, address) = self.next_sponsor();
        let sig = key.sign(payload);
        (address, sig.to_bytes().to_vec())
    }
}
