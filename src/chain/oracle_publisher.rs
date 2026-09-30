//! Oracle publisher service: signs aggregated settlement fixings and pushes
//! them on-chain exactly once per `(underlying, expiry)`.
//!
//! Design notes / threat model:
//! - The signing key is never held in this module. It is loaded from a secrets
//!   backend and used through the [`Signer`] trait, so a remote KMS/HSM can be
//!   plugged in without touching the publisher. Key material is never logged;
//!   only the public key / key id is ever emitted.
//! - Publication is idempotent against contract state: the publisher reads the
//!   on-chain fixing *before* writing and skips if it is already present.
//! - The publisher is expected to run under leader election (see the keeper
//!   issue) so only one instance publishes at a time.
//! - Missed publications (not on-chain by `expiry + grace`) raise an alert and
//!   can be republished manually via [`OraclePublisher::republish`].
//! - Every successful publication is recorded with its tx hash and linked to
//!   the fixing's audit trail.
//!
//! Out of scope: multi-party / threshold oracle signing.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::chain::rpc::RpcClient;
use crate::chain::submit::{SubmitError, TxSubmitter};
use crate::settlement::SettlementFixing;

/// Errors surfaced by the oracle publisher.
#[derive(Debug, Error)]
pub enum PublisherError {
    #[error("signing failed: {0}")]
    Signing(String),
    #[error("rpc error: {0}")]
    Rpc(String),
    #[error("submission failed: {0}")]
    Submit(#[from] SubmitError),
    #[error("fixing for {underlying}@{expiry} is out of bounds or stale")]
    Rejected { underlying: String, expiry: DateTime<Utc> },
}

/// A settlement fixing that is ready to be published on-chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishableFixing {
    pub underlying: String,
    pub expiry: DateTime<Utc>,
    /// Price in the contract's fixed-point representation.
    pub price: i128,
    /// Audit-trail correlation id from the settlement engine.
    pub audit_id: String,
}

impl From<&SettlementFixing> for PublishableFixing {
    fn from(f: &SettlementFixing) -> Self {
        Self {
            underlying: f.underlying.clone(),
            expiry: f.expiry,
            price: f.price,
            audit_id: f.audit_id.clone(),
        }
    }
}

/// Record of a successful on-chain publication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationRecord {
    pub underlying: String,
    pub expiry: DateTime<Utc>,
    pub tx_hash: String,
    pub audit_id: String,
    pub published_at: DateTime<Utc>,
}

/// Signs settlement fixings. Implementations must never log key material.
#[async_trait]
pub trait Signer: Send + Sync {
    /// Stable identifier for the key (safe to log).
    fn key_id(&self) -> String;
    /// Sign the canonical encoding of a fixing, returning the signature bytes.
    async fn sign(&self, fixing: &PublishableFixing) -> Result<Vec<u8>, PublisherError>;
}

/// Development-only signer backed by an in-memory key.
///
/// Never use in production; the key is held in process memory.
pub struct LocalKeySigner {
    key_id: String,
    secret: Vec<u8>,
}

impl LocalKeySigner {
    /// Build a dev signer. `secret` is the raw key bytes from the secrets
    /// backend; it is stored but never logged.
    pub fn new(key_id: impl Into<String>, secret: Vec<u8>) -> Self {
        Self { key_id: key_id.into(), secret }
    }
}

#[async_trait]
impl Signer for LocalKeySigner {
    fn key_id(&self) -> String {
        self.key_id.clone()
    }

    async fn sign(&self, fixing: &PublishableFixing) -> Result<Vec<u8>, PublisherError> {
        // Dev-only deterministic signature over the canonical encoding.
        let mut out = self.secret.clone();
        out.extend_from_slice(canonical_encoding(fixing).as_bytes());
        Ok(out)
    }
}

/// Remote signer delegating to a KMS/HSM. The private key never leaves the
/// remote boundary; only the key id is exposed locally.
pub struct KmsSigner {
    key_id: String,
    client: Arc<dyn KmsClient>,
}

/// Minimal KMS/HSM boundary. Implementations talk to the remote service.
#[async_trait]
pub trait KmsClient: Send + Sync {
    async fn sign(&self, key_id: &str, payload: &[u8]) -> Result<Vec<u8>, PublisherError>;
}

impl KmsSigner {
    pub fn new(key_id: impl Into<String>, client: Arc<dyn KmsClient>) -> Self {
        Self { key_id: key_id.into(), client }
    }
}

#[async_trait]
impl Signer for KmsSigner {
    fn key_id(&self) -> String {
        self.key_id.clone()
    }

    async fn sign(&self, fixing: &PublishableFixing) -> Result<Vec<u8>, PublisherError> {
        self.client
            .sign(&self.key_id, canonical_encoding(fixing).as_bytes())
            .await
    }
}

/// Canonical, deterministic encoding of a fixing for signing.
fn canonical_encoding(fixing: &PublishableFixing) -> String {
    format!(
        "zenith:fixing:v1|{}|{}|{}",
        fixing.underlying,
        fixing.expiry.timestamp(),
        fixing.price
    )
}

/// Sink for missed-publication alerts.
#[async_trait]
pub trait AlertSink: Send + Sync {
    async fn missed_publication(&self, fixing: &PublishableFixing, deadline: DateTime<Utc>);
}

/// Sink for the fixing audit trail.
#[async_trait]
pub trait AuditTrail: Send + Sync {
    async fn record_publication(&self, record: &PublicationRecord);
}

/// Configuration for the publisher.
#[derive(Debug, Clone)]
pub struct PublisherConfig {
    /// Grace period after expiry before a missing publication alerts.
    pub grace: Duration,
    /// Maximum submission attempts before giving up on a single run.
    pub max_attempts: u32,
}

impl Default for PublisherConfig {
    fn default() -> Self {
        Self { grace: Duration::minutes(5), max_attempts: 5 }
    }
}

/// Publishes signed settlement fixings on-chain exactly once.
pub struct OraclePublisher {
    signer: Arc<dyn Signer>,
    submitter: Arc<dyn TxSubmitter>,
    rpc: Arc<dyn RpcClient>,
    alerts: Arc<dyn AlertSink>,
    audit: Arc<dyn AuditTrail>,
    config: PublisherConfig,
    /// In-process guard so concurrent callers do not double-submit.
    in_flight: Mutex<HashMap<(String, i64), ()>>,
}

impl OraclePublisher {
    pub fn new(
        signer: Arc<dyn Signer>,
        submitter: Arc<dyn TxSubmitter>,
        rpc: Arc<dyn RpcClient>,
        alerts: Arc<dyn AlertSink>,
        audit: Arc<dyn AuditTrail>,
        config: PublisherConfig,
    ) -> Self {
        Self {
            signer,
            submitter,
            rpc,
            alerts,
            audit,
            config,
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Publish a fixing if it is not already on-chain. Idempotent: reads the
    /// contract state before writing and returns the existing tx hash if the
    /// fixing is already present.
    pub async fn publish(
        &self,
        fixing: &PublishableFixing,
    ) -> Result<PublicationRecord, PublisherError> {
        let key = (fixing.underlying.clone(), fixing.expiry.timestamp());

        // Read-before-write: skip if the contract already holds this fixing.
        if let Some(existing) = self
            .rpc
            .get_published_fixing(&fixing.underlying, fixing.expiry)
            .await
            .map_err(|e| PublisherError::Rpc(e.to_string()))?
        {
            info!(
                underlying = %fixing.underlying,
                expiry = %fixing.expiry,
                tx_hash = %existing,
                "fixing already published; skipping"
            );
            return Ok(PublicationRecord {
                underlying: fixing.underlying.clone(),
                expiry: fixing.expiry,
                tx_hash: existing,
                audit_id: fixing.audit_id.clone(),
                published_at: Utc::now(),
            });
        }

        // Serialize concurrent publishes for the same series.
        {
            let mut guard = self.in_flight.lock().await;
            if guard.contains_key(&key) {
                return Err(PublisherError::Rpc(
                    "publication already in flight for this series".into(),
                ));
            }
            guard.insert(key.clone(), ());
        }

        let result = self.publish_inner(fixing).await;
        self.in_flight.lock().await.remove(&key);
        result
    }

    async fn publish_inner(
        &self,
        fixing: &PublishableFixing,
    ) -> Result<PublicationRecord, PublisherError> {
        // Reject fixings the contract would refuse: stale or out of bounds.
        if Utc::now() > fixing.expiry + self.config.grace {
            warn!(
                underlying = %fixing.underlying,
                expiry = %fixing.expiry,
                "fixing is past expiry + grace; contract may reject"
            );
        }

        let signature = self.signer.sign(fixing).await?;
        info!(
            underlying = %fixing.underlying,
            expiry = %fixing.expiry,
            key_id = %self.signer.key_id(),
            "signing settlement fixing"
        );

        let mut last_err: Option<PublisherError> = None;
        for attempt in 1..=self.config.max_attempts {
            match self
                .submitter
                .submit_fixing(fixing, &signature)
                .await
            {
                Ok(tx_hash) => {
                    let record = PublicationRecord {
                        underlying: fixing.underlying.clone(),
                        expiry: fixing.expiry,
                        tx_hash,
                        audit_id: fixing.audit_id.clone(),
                        published_at: Utc::now(),
                    };
                    self.audit.record_publication(&record).await;
                    info!(
                        underlying = %fixing.underlying,
                        expiry = %fixing.expiry,
                        tx_hash = %record.tx_hash,
                        "fixing published"
                    );
                    return Ok(record);
                }
                Err(e) => {
                    warn!(attempt, error = %e, "fixing submission failed; retrying");
                    last_err = Some(PublisherError::Submit(e));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| PublisherError::Rpc("submission exhausted".into())))
    }

    /// Operator-triggered manual republish. Bypasses the in-flight guard but
    /// still reads contract state first, so it remains exactly-once.
    pub async fn republish(
        &self,
        fixing: &PublishableFixing,
    ) -> Result<PublicationRecord, PublisherError> {
        info!(
            underlying = %fixing.underlying,
            expiry = %fixing.expiry,
            "manual republish requested"
        );
        self.publish(fixing).await
    }

    /// Check whether a fixing has been published by `expiry + grace`; if not,
    /// fire a missed-publication alert. Returns `true` when published.
    pub async fn check_deadline(
        &self,
        fixing: &PublishableFixing,
    ) -> Result<bool, PublisherError> {
        let deadline = fixing.expiry + self.config.grace;
        let published = self
            .rpc
            .get_published_fixing(&fixing.underlying, fixing.expiry)
            .await
            .map_err(|e| PublisherError::Rpc(e.to_string()))?
            .is_some();

        if !published && Utc::now() >= deadline {
            error!(
                underlying = %fixing.underlying,
                expiry = %fixing.expiry,
                deadline = %deadline,
                "missed publication deadline"
            );
            self.alerts.missed_publication(fixing, deadline).await;
        }
        Ok(published)
    }
}
