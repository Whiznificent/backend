use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
pub struct ChallengeQuery {
    pub account: String,
    pub home_domain: Option<String>,
    pub client_domain: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChallengeResponse {
    pub transaction: String,
    pub network_passphrase: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyRequest {
    pub transaction: String,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub token: String,
}

pub struct Sep10Server {
    signing_key: SigningKey,
    server_address: String,
    home_domain: String,
}

impl Sep10Server {
    pub fn new(signing_key: SigningKey, home_domain: String) -> Self {
        let server_address = crate::strkey::encode_stellar_public_key(signing_key.verifying_key().as_bytes());
        Self {
            signing_key,
            server_address,
            home_domain,
        }
    }

    pub fn build_challenge(&self, client_account: &str, network_passphrase: &str) -> ChallengeResponse {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let min_time = now;
        let max_time = now + 300; // 5 minute validity

        let mut nonce = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut nonce);
        let nonce_b64 = data_encoding::BASE64.encode(&nonce);

        // Challenge format representation
        let payload = format!(
            "SEP10_CHALLENGE:{}:{}:{}:{}:{}:{}",
            self.server_address,
            client_account,
            min_time,
            max_time,
            self.home_domain,
            nonce_b64
        );

        let server_sig = self.signing_key.sign(payload.as_bytes());
        let server_sig_b64 = data_encoding::BASE64.encode(&server_sig.to_bytes());

        let transaction_envelope = format!("{payload}:{server_sig_b64}");

        ChallengeResponse {
            transaction: transaction_envelope,
            network_passphrase: network_passphrase.to_string(),
        }
    }

    pub fn verify_challenge(
        &self,
        envelope: &str,
        client_signature_b64: &str,
    ) -> Result<String, StatusCode> {
        let parts: Vec<&str> = envelope.split(':').collect();
        if parts.len() < 7 {
            return Err(StatusCode::BAD_REQUEST);
        }

        let server_addr = parts[1];
        let client_addr = parts[2];
        let min_time: u64 = parts[3].parse().map_err(|_| StatusCode::BAD_REQUEST)?;
        let max_time: u64 = parts[4].parse().map_err(|_| StatusCode::BAD_REQUEST)?;
        let server_sig_b64 = parts[7];

        if server_addr != self.server_address {
            return Err(StatusCode::UNAUTHORIZED);
        }

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        if now < min_time || now > max_time {
            return Err(StatusCode::BAD_REQUEST); // Expired challenge
        }

        // Verify server signature
        let payload = format!("{}:{}:{}:{}:{}:{}", parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6]);
        let server_sig_bytes = data_encoding::BASE64.decode(server_sig_b64.as_bytes()).map_err(|_| StatusCode::BAD_REQUEST)?;
        let server_sig = ed25519_dalek::Signature::from_slice(&server_sig_bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        self.signing_key.verifying_key().verify_strict(payload.as_bytes(), &server_sig).map_err(|_| StatusCode::UNAUTHORIZED)?;

        // Verify client signature
        let client_pubkey_bytes = crate::strkey::decode_stellar_public_key(client_addr).map_err(|_| StatusCode::BAD_REQUEST)?;
        let client_verifying_key = VerifyingKey::from_bytes(&client_pubkey_bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        let client_sig_bytes = data_encoding::BASE64.decode(client_signature_b64.as_bytes()).map_err(|_| StatusCode::BAD_REQUEST)?;
        let client_sig = ed25519_dalek::Signature::from_slice(&client_sig_bytes).map_err(|_| StatusCode::BAD_REQUEST)?;

        client_verifying_key.verify_strict(envelope.as_bytes(), &client_sig).map_err(|_| StatusCode::UNAUTHORIZED)?;

        Ok(client_addr.to_string())
    }
}

pub async fn get_sep10_challenge(
    State(state): State<crate::AppState>,
    Query(q): Query<ChallengeQuery>,
) -> Result<Json<ChallengeResponse>, StatusCode> {
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    let server_key = SigningKey::from_bytes(&seed);
    let server = Sep10Server::new(server_key, "zenith.finance".into());

    let resp = server.build_challenge(&q.account, &state.network.passphrase);
    Ok(Json(resp))
}

pub async fn get_stellar_toml() -> (axum::http::HeaderMap, String) {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "text/plain; charset=utf-8".parse().unwrap(),
    );
    let toml = r#"
VERSION = "2.0.0"
NETWORK_PASSPHRASE = "Test SDF Network ; September 2015"
WEB_AUTH_ENDPOINT = "https://api.zenith.finance/auth"
"#;
    (headers, toml.trim().to_string())
}
