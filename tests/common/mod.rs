//! Shared harness for integration tests: spins up the real router
//! (`build_router`) over a throwaway on-disk SQLite database, and offers
//! thin request/response helpers so each test file can stay focused on
//! what it's asserting instead of axum/tower plumbing.
//!
//! Each `tests/*_test.rs` file compiles this module into its own separate
//! binary, so a helper only some test files call (e.g. `get` without a
//! token, `delete_with`) looks "unused" from any one binary's point of
//! view even though it's exercised by others.
#![allow(dead_code)]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use serde_json::Value;
use std::net::SocketAddr;
use tower::ServiceExt;

pub struct TestApp {
    router: axum::Router,
    db_path: std::path::PathBuf,
    /// A clone of the AppState the router was built over. The `Arc<Mutex<_>>`
    /// maps inside are shared with the router, so a test can nudge spot
    /// prices/vols directly (deterministic inputs) without going through the
    /// WebSocket feed or the background simulator, which TestApp never spawns.
    pub state: zenith_backend::AppState,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
    }
}

impl TestApp {
    pub async fn spawn() -> Self {
        let db_path = std::env::temp_dir().join(format!("zenith-test-{}.db", uuid::Uuid::new_v4()));
        let database_url = format!("sqlite://{}", db_path.display());
        let pool = zenith_backend::db::init_pool(&database_url).await;
        let state = zenith_backend::AppState::new(pool);
        Self {
            router: zenith_backend::build_router(state.clone()),
            db_path,
            state,
        }
    }

    /// Overwrites the spot price (and optionally the base vol) for one
    /// underlying on the shared AppState — lets a test pin deterministic
    /// inputs instead of living with the seeded constants.
    pub fn set_spot(&self, underlying: &str, spot: f64) {
        self.state
            .spot_prices
            .lock()
            .unwrap()
            .insert(underlying.to_string(), spot);
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let (status, _headers, body) = self.send_raw(req).await;
        (status, body)
    }

    /// Like `send`, but also returns response headers — for tests that
    /// need to assert on something other than status/body (e.g.
    /// x-request-id propagation).
    async fn send_raw(&self, mut req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
        // Calling the router directly (rather than through axum::serve)
        // skips the connection layer that would normally populate this —
        // SmartIpKeyExtractor's peer-IP fallback needs it present the same
        // way a real TCP connection would provide it via
        // into_make_service_with_connect_info.
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));

        let response = self.router.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            // Not every response is JSON-bodied — tower_governor's 429s are
            // plain text ("Too Many Requests! Wait for Ns"). Fall back to
            // the raw text as a JSON string rather than unwrapping, so
            // tests that only care about `status` don't panic on those.
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, headers, body)
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.get_with(path, None).await
    }

    /// GET with an optional bearer token and an arbitrary extra header,
    /// returning response headers too.
    pub async fn get_raw(
        &self,
        path: &str,
        token: Option<&str>,
        extra_header: Option<(&str, &str)>,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut builder = Request::builder().method("GET").uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        if let Some((name, value)) = extra_header {
            builder = builder.header(name, value);
        }
        self.send_raw(builder.body(Body::empty()).unwrap()).await
    }

    pub async fn get_with(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method("GET").uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.post_with(path, body, None).await
    }

    pub async fn post_with(
        &self,
        path: &str,
        body: Value,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        self.send(builder.body(Body::from(body.to_string())).unwrap())
            .await
    }

    pub async fn delete_with(&self, path: &str, token: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .method("DELETE")
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        self.send(req).await
    }

    /// Full sign-in-with-wallet flow for a fresh random keypair, returning
    /// a bearer token ready to use in Authorization headers.
    pub async fn login(&self) -> String {
        self.login_account().await.1
    }

    /// Same as `login`, but also hands back the wallet's Stellar address so a
    /// test can correlate the session with the `wallet_address` column on the
    /// rows/account it owns (e.g. cross-wallet isolation invariants).
    pub async fn login_account(&self) -> (String, String) {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let signing_key = SigningKey::from_bytes(&seed);
        let address = zenith_backend::strkey::encode_stellar_public_key(
            signing_key.verifying_key().as_bytes(),
        );

        let (_, nonce_resp) = self
            .post(
                "/api/v1/auth/nonce",
                serde_json::json!({ "wallet_address": address }),
            )
            .await;
        let message = nonce_resp["message"].as_str().unwrap();
        let signature = signing_key.sign(message.as_bytes());
        let sig_b64 = data_encoding::BASE64.encode(&signature.to_bytes());

        let (_, verify_resp) = self
            .post(
                "/api/v1/auth/verify",
                serde_json::json!({ "wallet_address": address, "message": message, "signature": sig_b64 }),
            )
            .await;
        let token = verify_resp["token"].as_str().unwrap().to_string();
        (address, token)
    }
}
