//! Optional caching layer for expensive, shareable computations: option
//! chains, the spot/vol surface, expiry calendars, protocol stats and wallet
//! readiness.
//!
//! Two implementations behind the [`Cache`] trait:
//!
//! * [`MokaCache`] — the default, an in-process cache. No network, no
//!   serialization, per-replica.
//! * [`RedisCache`] — a shared cache so several API replicas don't each
//!   recompute the same chain between ticks.
//!
//! Cache keys for market data include the market snapshot version (see
//! `AppState::market_version`), so a new tick makes the previous tick's
//! entries unreachable — no explicit invalidation or delete is needed for
//! market data; the old entries simply expire via their TTL.
//!
//! [`CacheService`] adds stampede protection (single-flight per key) and
//! hit/miss metrics per key family on top of either backend. A Redis outage
//! degrades to computing on every request, never to an error: [`RedisCache`]
//! swallows connection failures and reports a miss.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A low-level byte cache. Implementations must be cheap to clone and safe to
/// share across tasks.
#[axum::async_trait]
pub trait Cache: Send + Sync {
    /// Returns the cached bytes for `key`, or `None` on a miss or any backend
    /// error (a Redis outage is a miss, not an error).
    async fn get(&self, key: &str) -> Option<Vec<u8>>;
    /// Stores `value` under `key`. Implementations may apply a TTL and may
    /// drop the write on a backend error (best-effort).
    async fn set(&self, key: &str, value: Vec<u8>);
    /// Removes `key`. Used to invalidate per-wallet entries (e.g. readiness)
    /// when the underlying data changes on a trade — unlike market data,
    /// these are keyed by wallet, not by snapshot version.
    async fn remove(&self, key: &str);
}

/// In-process cache backed by moka. The default: no network round-trip, no
/// serialization, but per-replica (each API instance computes its own chains).
pub struct MokaCache {
    inner: moka::sync::Cache<String, Vec<u8>>,
}

impl MokaCache {
    pub fn new(max_capacity: u64, ttl: Duration) -> Self {
        let inner = moka::sync::CacheBuilder::new(max_capacity)
            .time_to_live(ttl)
            .build();
        Self { inner }
    }
}

#[axum::async_trait]
impl Cache for MokaCache {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.inner.get(key)
    }

    async fn set(&self, key: &str, value: Vec<u8>) {
        self.inner.insert(key.to_string(), value);
    }

    async fn remove(&self, key: &str) {
        self.inner.invalidate(key);
    }
}

/// Shared cache backed by Redis. Survives across replicas; on any connection
/// failure it reports a miss / drops the write so the caller computes instead
/// of erroring.
pub struct RedisCache {
    client: redis::Client,
}

impl RedisCache {
    pub fn new(url: &str) -> Self {
        let client = redis::Client::open(url).expect("invalid Redis URL");
        Self { client }
    }
}

#[axum::async_trait]
impl Cache for RedisCache {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        let mut conn = self.client.get_multiplexed_async_connection().await.ok()?;
        use redis::AsyncCommands;
        conn.get(key).await.ok().flatten()
    }

    async fn set(&self, key: &str, value: Vec<u8>) {
        if let Ok(mut conn) = self.client.get_multiplexed_async_connection().await {
            use redis::AsyncCommands;
            let _: Result<(), _> = conn.set(key, value).await;
        }
    }

    async fn remove(&self, key: &str) {
        if let Ok(mut conn) = self.client.get_multiplexed_async_connection().await {
            use redis::AsyncCommands;
            let _: Result<(), _> = conn.del(key).await;
        }
    }
}

/// Hit/miss counters per key family, for the hit/miss ratio metrics.
#[derive(Default)]
pub struct Metrics {
    families: std::sync::Mutex<HashMap<String, FamilyMetrics>>,
}

#[derive(Default)]
struct FamilyMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Metrics {
    fn record_hit(&self, family: &str) {
        let families = self.families.lock().unwrap();
        families.get(family).map(|f| f.hits.fetch_add(1, Ordering::Relaxed));
    }

    fn record_miss(&self, family: &str) {
        let families = self.families.lock().unwrap();
        families.get(family).map(|f| f.misses.fetch_add(1, Ordering::Relaxed));
    }

    /// Returns `(hits, misses)` per family. Intended for a future metrics
    /// endpoint or log line; the data is kept in-process.
    pub fn snapshot(&self) -> HashMap<String, (u64, u64)> {
        let families = self.families.lock().unwrap();
        families
            .iter()
            .map(|(k, f)| {
                (
                    k.clone(),
                    (
                        f.hits.load(Ordering::Relaxed),
                        f.misses.load(Ordering::Relaxed),
                    ),
                )
            })
            .collect()
    }

    fn ensure_family(&self, family: &str) {
        let mut families = self.families.lock().unwrap();
        families.entry(family.to_string()).or_default();
    }
}

/// High-level cache: single-flight stampede protection plus hit/miss metrics
/// per key family, over any [`Cache`] backend.
///
/// `get_or_compute` returns the cached value if present; otherwise exactly one
/// caller computes it (per key) while the rest wait for that result, then the
/// value is stored for subsequent calls.
pub struct CacheService {
    inner: Arc<dyn Cache>,
    /// Per-key in-flight slots. The first caller to a key computes while
    /// holding that key's mutex; later callers block on it, then double-check
    /// the cache (now populated) instead of computing again.
    inflight: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Vec<u8>>>>>>>,
    metrics: Arc<Metrics>,
}

impl CacheService {
    pub fn new(inner: Arc<dyn Cache>) -> Self {
        Self {
            inner,
            inflight: Arc::new(std::sync::Mutex::new(HashMap::new())),
            metrics: Arc::new(Metrics::default()),
        }
    }

    /// Returns the metrics handle (e.g. to expose a hit/miss ratio).
    pub fn metrics(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    /// Returns the cached value for `key`, computing it via `f` (once, shared
    /// among concurrent callers) on a miss. `family` namespaces the metrics.
    pub async fn get_or_compute<F, Fut, T>(&self, family: &str, key: &str, f: F) -> T
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = T> + Send,
        T: Serialize + for<'de> Deserialize<'de> + Clone + Send,
    {
        self.metrics.ensure_family(family);

        // Fast path: a populated cache.
        if let Some(bytes) = self.inner.get(key).await {
            self.metrics.record_hit(family);
            return serde_json::from_slice(&bytes).expect("cached value deserializes");
        }
        self.metrics.record_miss(family);

        // Stampede protection: one computation per key at a time.
        let slot = {
            let mut inflight = self.inflight.lock().unwrap();
            inflight
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
                .clone()
        };

        let mut guard = slot.lock().await;
        // Double-check: a waiter that held the slot before us may have
        // populated the cache while we were waiting for the lock.
        if let Some(bytes) = self.inner.get(key).await {
            self.metrics.record_hit(family);
            return serde_json::from_slice(&bytes).expect("cached value deserializes");
        }

        let value = f().await;
        let bytes =
            serde_json::to_vec(&value).expect("cacheable value serializes");
        self.inner.set(key, bytes).await;
        *guard = Some(value.clone());
        drop(guard);
        self.inflight.lock().unwrap().remove(key);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_or_compute_caches_then_reuses() {
        let cache = CacheService::new(Arc::new(MokaCache::new(100, Duration::from_secs(60))));
        let metrics = cache.metrics();

        // First call misses and computes.
        let v1 = cache.get_or_compute("test", "k1", || async { 42 }).await;
        assert_eq!(v1, 42);
        // Second call hits the cache and must not recompute (the closure would
        // panic if it ran).
        let v2 = cache
            .get_or_compute("test", "k1", || async {
                panic!("must be served from cache");
            })
            .await;
        assert_eq!(v2, 42);

        let snap = metrics.snapshot();
        assert_eq!(snap.get("test"), Some(&(1, 1)), "one hit, one miss: {snap:?}");
    }

    #[tokio::test]
    async fn remove_invalidates_a_key() {
        let cache = CacheService::new(Arc::new(MokaCache::new(100, Duration::from_secs(60))));
        let metrics = cache.metrics();

        let v1 = cache.get_or_compute("test", "k1", || async { 1 }).await;
        assert_eq!(v1, 1);
        // Served from cache, not recomputed.
        let v2 = cache.get_or_compute("test", "k1", || async { 2 }).await;
        assert_eq!(v2, 1);
        assert_eq!(metrics.snapshot().get("test"), Some(&(1, 1)));

        // Removing the key forces a recompute.
        cache.remove("k1").await;
        let v3 = cache.get_or_compute("test", "k1", || async { 2 }).await;
        assert_eq!(v3, 2);
        assert_eq!(metrics.snapshot().get("test"), Some(&(1, 2)));
    }
}
