//! Pluggable secret management (issue #120).
//!
//! Everything secret — DB credentials, HMAC keys for cursors/idempotency/webhook
//! signatures, the oracle/sponsor/SEP-10 signing keys, third-party API tokens —
//! is fetched through a [`SecretProvider`] rather than read ad hoc from the
//! environment. Three backends ship today:
//!
//! * [`EnvSecretProvider`] — the dev/default source: one env var per secret.
//! * [`SopsFileSecretProvider`] — a SOPS-encrypted file, decrypted on demand by
//!   the `sops` binary (never decrypted to disk by us).
//! * [`VaultSecretProvider`] — HashiCorp Vault KV v2, with token-lease renewal.
//!
//! Values are always [`secrecy::SecretString`]: redacted in `Debug`, zeroised on
//! drop, and only exposed through `ExposeSecret` at the exact point of use.
//!
//! [`SecretStore`] fetches the configured set at startup (failing fast if the
//! backend is unavailable) and refreshes it on a timer, so a rotation or a
//! transient backend outage at runtime never takes the process down — it keeps
//! serving the cached values and alerts.
//!
//! Remote *signing* lives in [`crate::signing`]; this module owns retrieval.
//! The rotation procedure for each secret class is in `docs/secrets-rotation.md`.

pub mod hmac_ring;
pub mod sigv4;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

pub use secrecy::{ExposeSecret, SecretString};

/// Why a secret could not be retrieved. Messages name the secret/backend,
/// never a secret *value*.
#[derive(Debug)]
pub enum SecretError {
    /// The backend was reachable but has no such secret.
    NotFound(String),
    /// The backend could not be reached, or refused the credentials.
    Unavailable(String),
    /// The backend answered but the payload was unusable.
    Backend(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::NotFound(name) => write!(f, "secret {name} was not found"),
            SecretError::Unavailable(what) => write!(f, "secret backend unavailable: {what}"),
            SecretError::Backend(detail) => write!(f, "secret backend error: {detail}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// A source of named secrets.
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Fetch the current value of `name`.
    async fn get(&self, name: &str) -> Result<SecretString, SecretError>;

    /// True for backends that hand out time-limited leases (Vault) and whose
    /// credentials therefore need periodic renewal.
    fn supports_lease_renewal(&self) -> bool {
        false
    }

    /// Renew the backend's lease/credentials. Default no-op for static
    /// backends like env vars and SOPS files.
    async fn renew_lease(&self) -> Result<(), SecretError> {
        Ok(())
    }
}

// ─── HTTP transport ───────────────────────────────────────────────────────────

/// The tiny slice of HTTP the Vault/KMS clients need. Abstracting it lets the
/// provider/signer unit tests drive every response shape (200, 404, 5xx,
/// malformed JSON, token rejection) without a live server or testcontainers,
/// while production uses [`ReqwestHttpClient`].
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<(u16, String), SecretError>;

    async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &Value,
    ) -> Result<(u16, String), SecretError>;
}

/// Production [`HttpClient`] backed by `reqwest` (rustls; no OpenSSL).
pub struct ReqwestHttpClient {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

async fn read_response(response: reqwest::Response) -> Result<(u16, String), SecretError> {
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| SecretError::Unavailable(format!("reading response body: {e}")))?;
    Ok((status, body))
}

#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<(u16, String), SecretError> {
        let mut request = self.client.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        read_response(
            request
                .send()
                .await
                .map_err(|e| SecretError::Unavailable(e.to_string()))?,
        )
        .await
    }

    async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &Value,
    ) -> Result<(u16, String), SecretError> {
        let mut request = self.client.post(url).json(body);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        read_response(
            request
                .send()
                .await
                .map_err(|e| SecretError::Unavailable(e.to_string()))?,
        )
        .await
    }
}

// ─── SecretStore ──────────────────────────────────────────────────────────────

/// A cached view of the configured secrets, backed by a [`SecretProvider`].
///
/// `load` fetches everything up front and fails fast — a process that can't
/// reach its secret backend at startup must not come up half-configured.
/// `refresh_loop` re-fetches on a timer; if the backend is briefly unavailable
/// it keeps serving the last known values (and logs) instead of dropping to a
/// broken state mid-flight.
pub struct SecretStore {
    provider: Arc<dyn SecretProvider>,
    cache: tokio::sync::RwLock<HashMap<String, Arc<SecretString>>>,
    refresh_interval: Duration,
}

impl SecretStore {
    /// Fetch `names` immediately, failing fast if any is missing.
    pub async fn load(
        provider: Arc<dyn SecretProvider>,
        names: &[&str],
        refresh_interval: Duration,
    ) -> Result<Self, SecretError> {
        let store = Self {
            provider,
            cache: tokio::sync::RwLock::new(HashMap::new()),
            refresh_interval,
        };
        store.refresh(names).await?;
        Ok(store)
    }

    /// The cached value for `name`, fetching it once if it isn't cached yet.
    pub async fn get(&self, name: &str) -> Result<Arc<SecretString>, SecretError> {
        if let Some(cached) = self.cache.read().await.get(name) {
            return Ok(Arc::clone(cached));
        }
        let value = Arc::new(self.provider.get(name).await?);
        self.cache
            .write()
            .await
            .insert(name.to_string(), Arc::clone(&value));
        Ok(value)
    }

    /// Re-fetch every requested name and swap the cache only if *all* succeed,
    /// so a partial backend failure can't leave a mixed-generation cache.
    pub async fn refresh(&self, names: &[&str]) -> Result<(), SecretError> {
        let mut fetched = HashMap::with_capacity(names.len());
        for name in names {
            fetched.insert(
                (*name).to_string(),
                Arc::new(self.provider.get(name).await?),
            );
        }
        let mut cache = self.cache.write().await;
        for (name, value) in fetched {
            cache.insert(name, value);
        }
        Ok(())
    }

    /// Refresh on a timer until the process exits. Never returns.
    pub async fn refresh_loop(self: Arc<Self>, names: Vec<String>) {
        let mut ticker = tokio::time::interval(self.refresh_interval);
        loop {
            ticker.tick().await;
            let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
            if let Err(error) = self.refresh(&borrowed).await {
                tracing::warn!(
                    error = %error,
                    "secret refresh failed; continuing to serve cached values"
                );
            }
            if self.provider.supports_lease_renewal() {
                if let Err(error) = self.provider.renew_lease().await {
                    tracing::warn!(error = %error, "secret lease renewal failed");
                }
            }
        }
    }
}

// ─── Env backend ──────────────────────────────────────────────────────────────

/// Dev/default backend: one environment variable per secret. Reads are cheap
/// and dynamic, so a rotated value is picked up on the next refresh.
#[derive(Default)]
pub struct EnvSecretProvider;

impl EnvSecretProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SecretProvider for EnvSecretProvider {
    async fn get(&self, name: &str) -> Result<SecretString, SecretError> {
        match std::env::var(name) {
            Ok(value) => Ok(SecretString::from(value)),
            Err(std::env::VarError::NotPresent) => Err(SecretError::NotFound(name.to_string())),
            Err(std::env::VarError::NotUnicode(_)) => Err(SecretError::Backend(format!(
                "env var {name} is not valid unicode"
            ))),
        }
    }
}

// ─── SOPS backend ─────────────────────────────────────────────────────────────

/// Reads secrets from a SOPS-encrypted file by shelling out to the `sops`
/// binary, which decrypts using the operator's key material. The plaintext is
/// never written to disk — only captured from the child's stdout.
pub struct SopsFileSecretProvider {
    path: std::path::PathBuf,
    binary: std::path::PathBuf,
}

impl SopsFileSecretProvider {
    /// Use the `sops` binary found on `PATH`.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            binary: std::path::PathBuf::from("sops"),
        }
    }

    /// Use a specific `sops` binary (tests inject a stub).
    pub fn with_binary(
        path: impl Into<std::path::PathBuf>,
        binary: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            path: path.into(),
            binary: binary.into(),
        }
    }
}

#[async_trait]
impl SecretProvider for SopsFileSecretProvider {
    async fn get(&self, name: &str) -> Result<SecretString, SecretError> {
        let output = tokio::process::Command::new(&self.binary)
            .arg("--decrypt")
            .arg("--extract")
            .arg(format!("[\"{name}\"]"))
            .arg(&self.path)
            .output()
            .await
            .map_err(|e| SecretError::Unavailable(format!("running sops: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::NotFound(format!(
                "{name} (sops exited {}: {})",
                output.status,
                stderr.trim()
            )));
        }

        let value = String::from_utf8(output.stdout)
            .map_err(|_| SecretError::Backend(format!("sops output for {name} was not UTF-8")))?;
        Ok(SecretString::from(
            value.trim_end_matches(['\n', '\r']).to_string(),
        ))
    }
}

// ─── Vault backend (KV v2) ────────────────────────────────────────────────────

/// HashiCorp Vault KV v2 provider. Each secret is a JSON object with a `value`
/// key (simplest shape that still allows Vault's metadata to evolve), read via
/// `GET /v1/{mount}/data/{name}` with the client token.
///
/// The token itself is a [`SecretString`] and supports `auth/token/renew-self`,
/// which is what [`SecretProvider::supports_lease_renewal`] advertises.
pub struct VaultSecretProvider {
    http: Arc<dyn HttpClient>,
    base_url: String,
    mount: String,
    token: SecretString,
}

impl VaultSecretProvider {
    pub fn new(
        http: Arc<dyn HttpClient>,
        base_url: impl Into<String>,
        mount: impl Into<String>,
        token: SecretString,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            mount: mount.into(),
            token,
        }
    }

    fn url(&self, suffix: &str) -> String {
        format!("{}/v1/{}", self.base_url.trim_end_matches('/'), suffix)
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        vec![(
            "X-Vault-Token".to_string(),
            self.token.expose_secret().to_string(),
        )]
    }
}

#[async_trait]
impl SecretProvider for VaultSecretProvider {
    async fn get(&self, name: &str) -> Result<SecretString, SecretError> {
        let url = self.url(&format!("{}/data/{name}", self.mount));
        let (status, body) = self.http.get(&url, &self.auth_headers()).await?;

        if status == 404 {
            return Err(SecretError::NotFound(name.to_string()));
        }
        if !(200..300).contains(&status) {
            return Err(SecretError::Unavailable(format!(
                "vault returned {status} reading {name}"
            )));
        }

        let payload: Value = serde_json::from_str(&body)
            .map_err(|e| SecretError::Backend(format!("vault response for {name}: {e}")))?;
        payload["data"]["data"]["value"]
            .as_str()
            .map(|value| SecretString::from(value.to_string()))
            .ok_or_else(|| {
                SecretError::Backend(format!("vault secret {name} has no data.data.value"))
            })
    }

    fn supports_lease_renewal(&self) -> bool {
        true
    }

    async fn renew_lease(&self) -> Result<(), SecretError> {
        let url = self.url("auth/token/renew-self");
        let (status, body) = self
            .http
            .post_json(&url, &self.auth_headers(), &serde_json::json!({}))
            .await?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(SecretError::Unavailable(format!(
                "vault token renewal returned {status}: {body}"
            )))
        }
    }
}

// ─── Configuration ────────────────────────────────────────────────────────────

/// Build the provider selected by `ZENITH_SECRET_BACKEND` (`env` by default),
/// so deployment configuration decides where secrets come from without a code
/// change. `env`/`vault`/`sops` are the supported values.
pub fn provider_from_env() -> Result<Arc<dyn SecretProvider>, SecretError> {
    let backend = std::env::var("ZENITH_SECRET_BACKEND").unwrap_or_else(|_| "env".to_string());
    match backend.as_str() {
        "env" => Ok(Arc::new(EnvSecretProvider::new())),
        "sops" => {
            let file = std::env::var("SOPS_FILE")
                .map_err(|_| SecretError::NotFound("SOPS_FILE".to_string()))?;
            Ok(Arc::new(SopsFileSecretProvider::new(file)))
        }
        "vault" => {
            let address = std::env::var("VAULT_ADDR")
                .map_err(|_| SecretError::NotFound("VAULT_ADDR".to_string()))?;
            let token = std::env::var("VAULT_TOKEN")
                .map_err(|_| SecretError::NotFound("VAULT_TOKEN".to_string()))?;
            let mount =
                std::env::var("VAULT_SECRET_MOUNT").unwrap_or_else(|_| "secret".to_string());
            Ok(Arc::new(VaultSecretProvider::new(
                Arc::new(ReqwestHttpClient::new()),
                address,
                mount,
                SecretString::from(token),
            )))
        }
        other => Err(SecretError::Backend(format!(
            "unknown ZENITH_SECRET_BACKEND {other:?}; expected env, sops or vault"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A scripted HttpClient: records every request and returns responses in
    /// order, so the Vault paths can be exercised without a live server.
    struct MockHttp {
        responses: Mutex<Vec<(u16, String)>>,
        requests: Mutex<Vec<(String, String, Option<Value>)>>,
    }

    impl MockHttp {
        fn new(responses: Vec<(u16, String)>) -> Self {
            Self {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl HttpClient for MockHttp {
        async fn get(
            &self,
            url: &str,
            headers: &[(String, String)],
        ) -> Result<(u16, String), SecretError> {
            self.requests
                .lock()
                .unwrap()
                .push((url.to_string(), format!("{headers:?}"), None));
            Ok(self.responses.lock().unwrap().remove(0))
        }

        async fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &Value,
        ) -> Result<(u16, String), SecretError> {
            self.requests.lock().unwrap().push((
                url.to_string(),
                format!("{headers:?}"),
                Some(body.clone()),
            ));
            Ok(self.responses.lock().unwrap().remove(0))
        }
    }

    #[tokio::test]
    async fn provider_from_env_selects_backends() {
        std::env::remove_var("ZENITH_SECRET_BACKEND");
        std::env::set_var("ZENITH_TEST_SELECTED", "selected");
        let provider = provider_from_env().unwrap();
        let value = provider.get("ZENITH_TEST_SELECTED").await.unwrap();
        assert_eq!(value.expose_secret(), "selected");
        std::env::remove_var("ZENITH_TEST_SELECTED");

        std::env::set_var("ZENITH_SECRET_BACKEND", "nonsense");
        assert!(matches!(provider_from_env(), Err(SecretError::Backend(_))));
        std::env::remove_var("ZENITH_SECRET_BACKEND");
    }

    #[test]
    fn secret_strings_debug_is_redacted() {
        let secret = SecretString::from("super-secret-value".to_string());
        let debug = format!("{secret:?}");
        assert!(
            !debug.contains("super-secret-value"),
            "Debug output must not contain the secret: {debug}"
        );
    }

    #[tokio::test]
    async fn env_provider_reads_and_misses() {
        std::env::set_var("ZENITH_TEST_SECRET", "hunter2");
        let provider = EnvSecretProvider::new();
        let value = provider.get("ZENITH_TEST_SECRET").await.unwrap();
        assert_eq!(value.expose_secret(), "hunter2");

        let missing = provider.get("ZENITH_TEST_MISSING").await;
        assert!(matches!(missing, Err(SecretError::NotFound(_))));
        std::env::remove_var("ZENITH_TEST_SECRET");
    }

    #[tokio::test]
    async fn store_fails_fast_when_a_secret_is_missing() {
        let store = SecretStore::load(
            Arc::new(EnvSecretProvider::new()),
            &["ZENITH_TEST_DEFINITELY_MISSING"],
            Duration::from_secs(60),
        )
        .await;
        assert!(matches!(store, Err(SecretError::NotFound(_))));
    }

    #[tokio::test]
    async fn store_caches_after_the_first_fetch() {
        let dir = std::env::temp_dir().join(format!("zenith-sops-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("sops");
        std::fs::write(&binary, "#!/bin/sh\nprintf 'value-from-sops\\n'\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let provider = Arc::new(SopsFileSecretProvider::with_binary(
            dir.join("secrets.yaml"),
            &binary,
        ));
        let store = SecretStore::load(provider, &["ANY"], Duration::from_secs(60))
            .await
            .unwrap();

        let first = store.get("ANY").await.unwrap();
        assert_eq!(first.expose_secret(), "value-from-sops");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn vault_reads_kv_v2_data_data_value() {
        let http = Arc::new(MockHttp::new(vec![(
            200,
            r#"{"data":{"data":{"value":"vault-value"},"metadata":{}}}"#.to_string(),
        )]));
        let provider = VaultSecretProvider::new(
            http.clone(),
            "https://vault.example:8200/",
            "secret",
            SecretString::from("vault-token".to_string()),
        );

        let value = provider.get("db/password").await.unwrap();
        assert_eq!(value.expose_secret(), "vault-value");

        let requests = http.requests.lock().unwrap();
        assert_eq!(
            requests[0].0,
            "https://vault.example:8200/v1/secret/data/db/password"
        );
        assert!(requests[0].1.contains("vault-token"));
    }

    #[tokio::test]
    async fn vault_maps_404_to_not_found() {
        let http = Arc::new(MockHttp::new(vec![(404, "{}".to_string())]));
        let provider = VaultSecretProvider::new(
            http,
            "https://vault.example:8200",
            "secret",
            SecretString::from("t".to_string()),
        );
        let result = provider.get("missing").await;
        assert!(matches!(result, Err(SecretError::NotFound(_))));
    }

    #[tokio::test]
    async fn vault_renews_its_token_lease() {
        let http = Arc::new(MockHttp::new(vec![(200, "{}".to_string())]));
        let provider = VaultSecretProvider::new(
            http.clone(),
            "https://vault.example:8200",
            "secret",
            SecretString::from("t".to_string()),
        );
        assert!(provider.supports_lease_renewal());
        provider.renew_lease().await.unwrap();
        let requests = http.requests.lock().unwrap();
        assert_eq!(
            requests[0].0,
            "https://vault.example:8200/v1/auth/token/renew-self"
        );
    }

    #[tokio::test]
    async fn sops_reports_a_failed_decrypt_as_not_found() {
        let dir = std::env::temp_dir().join(format!("zenith-sops-fail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("sops");
        std::fs::write(&binary, "#!/bin/sh\necho 'decryption failed' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let provider = SopsFileSecretProvider::with_binary(dir.join("secrets.yaml"), &binary);
        let result = provider.get("ANY").await;
        assert!(matches!(result, Err(SecretError::NotFound(_))));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
