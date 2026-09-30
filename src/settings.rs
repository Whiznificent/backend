//! Runtime configuration, read from environment variables with defaults that
//! work with no configuration at all (so `cargo run` and the Docker build
//! behave out of the box).

use std::time::Duration;

use crate::cache::{Cache, MokaCache, RedisCache};
use crate::cache::CacheService;

/// Which cache backend to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheBackend {
    /// In-process moka cache. The default: no network, no serialization.
    Moka,
    /// Shared Redis cache across replicas.
    Redis,
}

/// Runtime settings. Constructed once from the environment in
/// [`crate::init_state`] / [`crate::AppState::new`].
#[derive(Clone, Debug)]
pub struct Settings {
    pub cache_backend: CacheBackend,
    pub redis_url: Option<String>,
    /// How long a market-data entry lives. Market keys also embed the
    /// snapshot version, so this is a backstop to evict old ticks, not the
    /// primary invalidation mechanism.
    pub market_cache_ttl: Duration,
    /// Maximum number of entries in the in-process moka cache.
    pub moka_max_capacity: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cache_backend: CacheBackend::Moka,
            redis_url: None,
            market_cache_ttl: Duration::from_secs(60),
            moka_max_capacity: 10_000,
        }
    }
}

impl Settings {
    /// Reads settings from the environment, falling back to defaults for
    /// anything unset. `ZENITH_CACHE=redis` switches to the shared backend
    /// (with `ZENITH_REDIS_URL` pointing at the server).
    pub fn from_env() -> Self {
        let mut settings = Self::default();

        if let Ok(backend) = std::env::var("ZENITH_CACHE") {
            settings.cache_backend = match backend.to_ascii_lowercase().as_str() {
                "redis" => CacheBackend::Redis,
                "moka" => CacheBackend::Moka,
                _ => CacheBackend::Moka,
            };
        }
        if let Ok(url) = std::env::var("ZENITH_REDIS_URL") {
            settings.redis_url = Some(url);
        }
        if let Ok(ttl) = std::env::var("ZENITH_CACHE_TTL_SECS") {
            if let Ok(secs) = ttl.parse::<u64>() {
                settings.market_cache_ttl = Duration::from_secs(secs);
            }
        }
        if let Ok(cap) = std::env::var("ZENITH_CACHE_MOKA_CAPACITY") {
            if let Ok(cap) = cap.parse::<u64>() {
                settings.moka_max_capacity = cap;
            }
        }

        settings
    }

    /// Builds the configured cache backend wrapped in a [`CacheService`].
    /// Redis without a configured URL falls back to moka (a misconfiguration
    /// should degrade, not panic at startup).
    pub fn build_cache(&self) -> CacheService {
        let inner: std::sync::Arc<dyn Cache> = match self.cache_backend {
            CacheBackend::Redis => {
                if let Some(url) = &self.redis_url {
                    std::sync::Arc::new(RedisCache::new(url))
                } else {
                    std::sync::Arc::new(MokaCache::new(
                        self.moka_max_capacity,
                        self.market_cache_ttl,
                    ))
                }
            }
            CacheBackend::Moka => std::sync::Arc::new(MokaCache::new(
                self.moka_max_capacity,
                self.market_cache_ttl,
            )),
        };
        CacheService::new(inner)
    }
}
