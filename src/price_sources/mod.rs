//! Pluggable price-feed ingestion layer.
//!
//! This module defines the [`PriceSource`] abstraction that replaces the
//! seeded-constant / random-walk simulator in `prices.rs`. Concrete
//! implementations live in sibling modules:
//!
//! * [`simulated`] — the deterministic random-walk source used for local
//!   development and tests (the default).
//! * [`http`] — an HTTP spot ticker feed (e.g. CoinGecko / Binance public
//!   ticker) built on `reqwest` with rustls.
//! * [`reflector`] — the Stellar Reflector oracle contract, read through the
//!   Soroban RPC `simulateTransaction` path (`lastprice`).
//!
//! The active source is selected through configuration (see
//! [`PriceSourceConfig`]); the default remains [`SimulatedSource`] so local
//! development and the existing unit tests keep working unchanged.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod http;
pub mod reflector;
pub mod simulated;

pub use http::HttpTickerSource;
pub use reflector::ReflectorOracleSource;
pub use simulated::SimulatedSource;

/// Maximum time any single fetch is allowed to take.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// A single observed price for one symbol.
///
/// `source` identifies which [`PriceSource`] produced the tick and
/// `observed_at` is the UTC timestamp at which the price was observed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceTick {
    /// The observed price, expressed in quote currency per unit of the symbol.
    pub price: f64,
    /// Human-readable identifier of the producing source (e.g. `"simulated"`).
    pub source: String,
    /// UTC timestamp at which the price was observed.
    pub observed_at: DateTime<Utc>,
}

impl PriceTick {
    /// Construct a new tick, stamping it with the current UTC time.
    pub fn now(price: f64, source: impl Into<String>) -> Self {
        Self {
            price,
            source: source.into(),
            observed_at: Utc::now(),
        }
    }
}

/// Errors that can be produced while fetching prices.
#[derive(Debug, thiserror::Error)]
pub enum PriceError {
    /// The upstream source returned a rate-limit response (HTTP 429).
    #[error("price source rate limited")]
    RateLimited,
    /// The upstream response could not be parsed or was malformed.
    #[error("malformed price response: {0}")]
    Malformed(String),
    /// The fetch exceeded the configured timeout.
    #[error("price fetch timed out")]
    Timeout,
    /// A transport-level failure (network, DNS, TLS, ...).
    #[error("price source transport error: {0}")]
    Transport(String),
    /// Any other source-specific failure.
    #[error("price source error: {0}")]
    Other(String),
}

/// A pluggable source of spot prices.
///
/// Implementations must be safe to share across tasks (`Send + Sync`) and are
/// expected to honour [`FETCH_TIMEOUT`]. A failed fetch must return an `Err`
/// rather than an empty map so callers can preserve the last good price.
#[async_trait::async_trait]
pub trait PriceSource: Send + Sync {
    /// Fetch the latest price for each requested symbol.
    ///
    /// Symbols that the source cannot resolve may be omitted from the returned
    /// map; callers treat missing symbols as "no update" and keep the last
    /// good price.
    async fn fetch(
        &self,
        symbols: &[String],
    ) -> Result<HashMap<String, PriceTick>, PriceError>;

    /// Stable identifier for this source, used to populate [`PriceTick::source`]
    /// and the `/api/v1/spot` `source` field.
    fn name(&self) -> &'static str;
}

/// Configuration selecting the active price source.
///
/// Read from the `PRICE_SOURCE` environment variable (case-insensitive).
/// Unknown or unset values fall back to [`SimulatedSource`], preserving the
/// historical local-dev / test behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceSourceConfig {
    /// Deterministic random-walk simulator (default).
    Simulated,
    /// HTTP spot ticker feed.
    Http { base_url: String },
    /// Stellar Reflector oracle contract.
    Reflector { rpc_url: String, contract_id: String },
}

impl Default for PriceSourceConfig {
    fn default() -> Self {
        PriceSourceConfig::Simulated
    }
}

impl PriceSourceConfig {
    /// Resolve the active configuration from the environment.
    pub fn from_env() -> Self {
        let raw = std::env::var("PRICE_SOURCE").unwrap_or_default();
        match raw.trim().to_ascii_lowercase().as_str() {
            "http" | "http_ticker" | "ticker" => PriceSourceConfig::Http {
                base_url: std::env::var("PRICE_SOURCE_HTTP_URL")
                    .unwrap_or_else(|_| "https://api.coingecko.com/api/v3".to_string()),
            },
            "reflector" | "oracle" => PriceSourceConfig::Reflector {
                rpc_url: std::env::var("PRICE_SOURCE_RPC_URL")
                    .unwrap_or_else(|_| "https://soroban-testnet.stellar.org".to_string()),
                contract_id: std::env::var("PRICE_SOURCE_CONTRACT_ID").unwrap_or_default(),
            },
            _ => PriceSourceConfig::Simulated,
        }
    }

    /// Build the concrete [`PriceSource`] described by this configuration.
    pub fn build(&self) -> Arc<dyn PriceSource> {
        match self {
            PriceSourceConfig::Simulated => Arc::new(SimulatedSource::default()),
            PriceSourceConfig::Http { base_url } => {
                Arc::new(HttpTickerSource::new(base_url.clone()))
            }
            PriceSourceConfig::Reflector {
                rpc_url,
                contract_id,
            } => Arc::new(ReflectorOracleSource::new(
                rpc_url.clone(),
                contract_id.clone(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_simulated() {
        assert_eq!(PriceSourceConfig::default(), PriceSourceConfig::Simulated);
    }

    #[test]
    fn unknown_env_falls_back_to_simulated() {
        std::env::set_var("PRICE_SOURCE", "definitely-not-a-source");
        assert_eq!(PriceSourceConfig::from_env(), PriceSourceConfig::Simulated);
        std::env::remove_var("PRICE_SOURCE");
    }

    #[test]
    fn tick_now_stamps_source_and_time() {
        let tick = PriceTick::now(123.4, "simulated");
        assert_eq!(tick.price, 123.4);
        assert_eq!(tick.source, "simulated");
        assert!(tick.observed_at <= Utc::now());
    }
}
