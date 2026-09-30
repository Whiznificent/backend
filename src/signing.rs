//! Stellar ed25519 signing behind a [`Signer`] abstraction (issue #120).
//!
//! The fee-sponsor, oracle and SEP-10 keys control protocol funds and
//! settlement, so the private key must not live in this process's memory in
//! production. [`VaultTransitSigner`] and [`AwsKmsSigner`] call a remote
//! signing service per signature and only ever hold an identifier plus a
//! credential for that service; [`LocalEd25519Signer`] exists so development
//! and tests can sign in-process without a remote dependency.

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use crate::secrets::sigv4::{self, SignableRequest};
use crate::secrets::{HttpClient, SecretError};

/// Signs arbitrary byte payloads with the protocol's ed25519 identity.
#[async_trait]
pub trait Signer: Send + Sync {
    /// Raw 32-byte ed25519 public key.
    async fn public_key(&self) -> Result<Vec<u8>, SecretError>;
    /// Raw 64-byte ed25519 signature over `message`.
    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SecretError>;
}

/// In-process signer, for local development and tests. Not for production:
/// the seed lives in this process's memory.
pub struct LocalEd25519Signer {
    key: ed25519_dalek::SigningKey,
}

impl LocalEd25519Signer {
    pub fn from_seed(seed: &[u8]) -> Result<Self, SecretError> {
        let bytes: [u8; 32] = seed.try_into().map_err(|_| {
            SecretError::Backend("ed25519 seed must be exactly 32 bytes".to_string())
        })?;
        Ok(Self {
            key: ed25519_dalek::SigningKey::from_bytes(&bytes),
        })
    }

    pub fn from_secret(secret: &SecretString) -> Result<Self, SecretError> {
        Self::from_seed(secret.expose_secret().as_bytes())
    }
}

#[async_trait]
impl Signer for LocalEd25519Signer {
    async fn public_key(&self) -> Result<Vec<u8>, SecretError> {
        Ok(self.key.verifying_key().to_bytes().to_vec())
    }

    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SecretError> {
        use ed25519_dalek::Signer as _;
        Ok(self.key.sign(message).to_bytes().to_vec())
    }
}

// ─── Vault Transit ────────────────────────────────────────────────────────────

/// Signs through a Vault Transit ed25519 key: Vault does the signing, the key
/// material never leaves Vault. Only the transit key name and a Vault token
/// are held here.
pub struct VaultTransitSigner {
    http: Arc<dyn HttpClient>,
    base_url: String,
    mount: String,
    key_name: String,
    token: SecretString,
}

impl VaultTransitSigner {
    pub fn new(
        http: Arc<dyn HttpClient>,
        base_url: impl Into<String>,
        mount: impl Into<String>,
        key_name: impl Into<String>,
        token: SecretString,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            mount: mount.into(),
            key_name: key_name.into(),
            token,
        }
    }

    fn url(&self, suffix: &str) -> String {
        format!(
            "{}/v1/{}/{}",
            self.base_url.trim_end_matches('/'),
            self.mount,
            suffix
        )
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        vec![(
            "X-Vault-Token".to_string(),
            self.token.expose_secret().to_string(),
        )]
    }
}

#[async_trait]
impl Signer for VaultTransitSigner {
    async fn public_key(&self) -> Result<Vec<u8>, SecretError> {
        let url = self.url(&format!("keys/{}", self.key_name));
        let (status, body) = self.http.get(&url, &self.auth_headers()).await?;
        if !(200..300).contains(&status) {
            return Err(SecretError::Unavailable(format!(
                "vault transit key lookup returned {status}"
            )));
        }
        let payload: Value = serde_json::from_str(&body)
            .map_err(|e| SecretError::Backend(format!("vault transit key response: {e}")))?;
        let keys = payload["data"]["keys"].as_object().ok_or_else(|| {
            SecretError::Backend("vault transit response has no data.keys".into())
        })?;
        let encoded = keys
            .values()
            .next()
            .and_then(Value::as_str)
            .ok_or_else(|| SecretError::Backend("vault transit key set is empty".into()))?;
        data_encoding::BASE64
            .decode(encoded.as_bytes())
            .map_err(|e| SecretError::Backend(format!("vault transit public key not base64: {e}")))
    }

    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SecretError> {
        let url = self.url(&format!("sign/{}", self.key_name));
        let body = serde_json::json!({ "input": data_encoding::BASE64.encode(message) });
        let (status, response) = self
            .http
            .post_json(&url, &self.auth_headers(), &body)
            .await?;
        if !(200..300).contains(&status) {
            return Err(SecretError::Unavailable(format!(
                "vault transit sign returned {status}"
            )));
        }
        let payload: Value = serde_json::from_str(&response)
            .map_err(|e| SecretError::Backend(format!("vault transit sign response: {e}")))?;
        let signature = payload["data"]["signature"].as_str().ok_or_else(|| {
            SecretError::Backend("vault transit response has no signature".into())
        })?;
        // Vault returns "vault:v<version>:<base64>".
        let encoded = signature
            .rsplit(':')
            .next()
            .ok_or_else(|| SecretError::Backend("vault transit signature is malformed".into()))?;
        data_encoding::BASE64
            .decode(encoded.as_bytes())
            .map_err(|e| SecretError::Backend(format!("vault transit signature not base64: {e}")))
    }
}

// ─── AWS KMS ──────────────────────────────────────────────────────────────────

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Signs through an AWS KMS `ED25519_SHA_512` key. Requests are SigV4-signed
/// with an IAM access key; KMS performs the signature, so the ed25519 private
/// key never exists outside KMS.
pub struct AwsKmsSigner {
    http: Arc<dyn HttpClient>,
    endpoint: String,
    region: String,
    key_id: String,
    access_key: String,
    secret_key: SecretString,
}

impl AwsKmsSigner {
    pub fn new(
        http: Arc<dyn HttpClient>,
        region: impl Into<String>,
        key_id: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: SecretString,
    ) -> Self {
        let region = region.into();
        let endpoint = format!("https://kms.{region}.amazonaws.com/");
        Self {
            http,
            endpoint,
            region,
            key_id: key_id.into(),
            access_key: access_key.into(),
            secret_key,
        }
    }

    /// Override the endpoint (LocalStack, a VPC endpoint, tests).
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    async fn call(&self, target: &str, body: &Value) -> Result<Value, SecretError> {
        let amz_date = sigv4::amz_timestamp(now_unix());
        let payload = body.to_string();
        let base_headers = vec![
            (
                "content-type".to_string(),
                "application/x-amz-json-1.1".to_string(),
            ),
            ("x-amz-target".to_string(), target.to_string()),
        ];
        let signed = sigv4::sign(&SignableRequest {
            method: "POST",
            url: &self.endpoint,
            headers: base_headers.clone(),
            payload: payload.as_bytes(),
            region: &self.region,
            service: "kms",
            access_key: &self.access_key,
            secret_key: self.secret_key.expose_secret(),
            amz_date: &amz_date,
        })?;

        let mut headers = base_headers;
        headers.extend(signed);
        let (status, response) = self.http.post_json(&self.endpoint, &headers, body).await?;
        if status != 200 {
            return Err(SecretError::Unavailable(format!(
                "AWS KMS returned {status}: {response}"
            )));
        }
        serde_json::from_str(&response)
            .map_err(|e| SecretError::Backend(format!("AWS KMS response: {e}")))
    }
}

#[async_trait]
impl Signer for AwsKmsSigner {
    async fn public_key(&self) -> Result<Vec<u8>, SecretError> {
        let response = self
            .call(
                "TrentService.GetPublicKey",
                &serde_json::json!({ "KeyId": self.key_id.clone() }),
            )
            .await?;
        let encoded = response["PublicKey"].as_str().ok_or_else(|| {
            SecretError::Backend("AWS KMS GetPublicKey response has no PublicKey".into())
        })?;
        let der = data_encoding::BASE64
            .decode(encoded.as_bytes())
            .map_err(|e| SecretError::Backend(format!("AWS KMS public key not base64: {e}")))?;
        if der.len() == 44 && der.starts_with(&ED25519_SPKI_PREFIX) {
            Ok(der[12..].to_vec())
        } else {
            Err(SecretError::Backend(
                "AWS KMS public key is not an ed25519 SubjectPublicKeyInfo".to_string(),
            ))
        }
    }

    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, SecretError> {
        let response = self
            .call(
                "TrentService.Sign",
                &serde_json::json!({
                    "KeyId": self.key_id.clone(),
                    "Message": data_encoding::BASE64.encode(message),
                    "MessageType": "RAW",
                    "SigningAlgorithm": "ED25519_SHA_512",
                }),
            )
            .await?;
        let signature = response["Signature"]
            .as_str()
            .ok_or_else(|| SecretError::Backend("AWS KMS Sign response has no Signature".into()))?;
        data_encoding::BASE64
            .decode(signature.as_bytes())
            .map_err(|e| SecretError::Backend(format!("AWS KMS signature not base64: {e}")))
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    type RecordedRequest = (String, Vec<(String, String)>, Option<Value>);

    /// Scripted HTTP transport so every remote signer path can be exercised
    /// without a live Vault/AWS.
    struct MockHttp {
        responses: Mutex<Vec<(u16, String)>>,
        requests: Mutex<Vec<RecordedRequest>>,
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
                .push((url.to_string(), headers.to_vec(), None));
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
                headers.to_vec(),
                Some(body.clone()),
            ));
            Ok(self.responses.lock().unwrap().remove(0))
        }
    }

    #[tokio::test]
    async fn local_signer_round_trips_a_signature() {
        let signer = LocalEd25519Signer::from_seed(&[7u8; 32]).unwrap();
        let public = signer.public_key().await.unwrap();
        assert_eq!(public.len(), 32);

        let signature = signer.sign(b"hello").await.unwrap();
        assert_eq!(signature.len(), 64);

        let verifying =
            ed25519_dalek::VerifyingKey::from_bytes(public.as_slice().try_into().unwrap()).unwrap();
        let parsed = ed25519_dalek::Signature::from_bytes(signature.as_slice().try_into().unwrap());
        use ed25519_dalek::Verifier as _;
        assert!(verifying.verify(b"hello", &parsed).is_ok());
    }

    #[tokio::test]
    async fn local_signer_rejects_a_wrong_length_seed() {
        assert!(LocalEd25519Signer::from_seed(&[0u8; 16]).is_err());
    }

    #[tokio::test]
    async fn vault_transit_signer_parses_a_prefixed_signature() {
        let raw = [9u8; 64];
        let encoded = data_encoding::BASE64.encode(&raw);
        let http = Arc::new(MockHttp::new(vec![(
            200,
            format!(r#"{{"data":{{"signature":"vault:v1:{encoded}"}}}}"#),
        )]));
        let signer = VaultTransitSigner::new(
            http.clone(),
            "https://vault.example:8200",
            "transit",
            "protocol",
            SecretString::from("token".to_string()),
        );

        let signature = signer.sign(b"payload").await.unwrap();
        assert_eq!(signature, raw.to_vec());

        let requests = http.requests.lock().unwrap();
        assert_eq!(
            requests[0].0,
            "https://vault.example:8200/v1/transit/sign/protocol"
        );
    }

    #[tokio::test]
    async fn vault_transit_signer_reads_the_public_key() {
        let raw = [3u8; 32];
        let encoded = data_encoding::BASE64.encode(&raw);
        let http = Arc::new(MockHttp::new(vec![(
            200,
            format!(r#"{{"data":{{"keys":{{"1":"{encoded}"}}}}}}"#),
        )]));
        let signer = VaultTransitSigner::new(
            http,
            "https://vault.example:8200",
            "transit",
            "protocol",
            SecretString::from("token".to_string()),
        );
        assert_eq!(signer.public_key().await.unwrap(), raw.to_vec());
    }

    #[tokio::test]
    async fn aws_kms_signer_sends_a_sigv4_request_and_decodes_the_signature() {
        let raw = [4u8; 64];
        let encoded = data_encoding::BASE64.encode(&raw);
        let http = Arc::new(MockHttp::new(vec![(
            200,
            format!(r#"{{"Signature":"{encoded}"}}"#),
        )]));
        let signer = AwsKmsSigner::new(
            http.clone(),
            "us-east-1",
            "key-id",
            "AKIDEXAMPLE",
            SecretString::from("secret".to_string()),
        );

        let signature = signer.sign(b"payload").await.unwrap();
        assert_eq!(signature, raw.to_vec());

        let requests = http.requests.lock().unwrap();
        let (url, headers, body) = &requests[0];
        assert_eq!(url, "https://kms.us-east-1.amazonaws.com/");
        assert!(headers
            .iter()
            .any(|(name, value)| { name == "x-amz-target" && value == "TrentService.Sign" }));
        assert!(headers.iter().any(|(name, value)| {
            name == "Authorization" && value.starts_with("AWS4-HMAC-SHA256")
        }));
        let body = body.as_ref().unwrap();
        assert_eq!(body["SigningAlgorithm"], "ED25519_SHA_512");
        assert_eq!(body["MessageType"], "RAW");
    }

    #[tokio::test]
    async fn aws_kms_signer_strips_the_ed25519_spki_prefix() {
        let raw = [5u8; 32];
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(&raw);
        let encoded = data_encoding::BASE64.encode(&der);
        let http = Arc::new(MockHttp::new(vec![(
            200,
            format!(r#"{{"PublicKey":"{encoded}"}}"#),
        )]));
        let signer = AwsKmsSigner::new(
            http,
            "us-east-1",
            "key-id",
            "AKIDEXAMPLE",
            SecretString::from("secret".to_string()),
        );

        assert_eq!(signer.public_key().await.unwrap(), raw.to_vec());
    }

    #[tokio::test]
    async fn aws_kms_signer_surfaces_a_non_200() {
        let http = Arc::new(MockHttp::new(vec![(
            403,
            r#"{"__type":"AccessDenied"}"#.to_string(),
        )]));
        let signer = AwsKmsSigner::new(
            http,
            "us-east-1",
            "key-id",
            "AKIDEXAMPLE",
            SecretString::from("secret".to_string()),
        );
        assert!(matches!(
            signer.sign(b"payload").await,
            Err(SecretError::Unavailable(_))
        ));
    }
}
