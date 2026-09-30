//! Time-bound executable quotes (RFQ quote locking).
//!
//! A quote captures the exact premium a caller was shown for a given wallet,
//! legs, size and side, together with an expiry. Execution endpoints accept an
//! optional `quote_id` and, when it is still valid, execute at exactly the
//! quoted premium instead of re-pricing against the live loop.
//!
//! Storage choice: quotes are persisted server-side. This keeps the record
//! authoritative for replay protection (a single `UPDATE ... WHERE used_at IS
//! NULL` marks a quote consumed inside the execution transaction) and lets us
//! audit issued/consumed quotes. A stateless HMAC token would avoid the table
//! but cannot enforce single-use without shared state anyway, so the table is
//! the simpler, more auditable option.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default quote lifetime in seconds when the caller does not specify one.
pub const DEFAULT_QUOTE_TTL_SECS: u64 = 10;

/// Default maximum allowed spot deviation (in percent) between quote issue and
/// execution. Protects against latency arbitrage.
pub const DEFAULT_MAX_DEVIATION_PCT: f64 = 0.5;

/// Error codes surfaced to API callers as `409 Conflict`.
pub const QUOTE_EXPIRED: &str = "QUOTE_EXPIRED";
pub const QUOTE_INVALID: &str = "QUOTE_INVALID";

/// A single leg of a quoted position or strategy.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteLeg {
    pub series: String,
    pub side: Side,
    pub size: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// Request payload for `POST /api/v1/quotes`.
#[derive(Debug, Clone)]
pub struct QuoteRequest {
    pub wallet: String,
    pub legs: Vec<QuoteLeg>,
    /// Optional explicit TTL; falls back to [`DEFAULT_QUOTE_TTL_SECS`].
    pub ttl_secs: Option<u64>,
}

/// A stored, executable quote.
#[derive(Debug, Clone)]
pub struct Quote {
    pub id: String,
    pub wallet: String,
    pub legs: Vec<QuoteLeg>,
    /// Total premium quoted at issue time.
    pub premium: f64,
    /// Spot price observed when the quote was issued.
    pub spot_at_issue: f64,
    /// Unix timestamp (seconds) after which the quote is no longer valid.
    pub expires_at: u64,
    /// Unix timestamp (seconds) at which the quote was consumed, if any.
    pub used_at: Option<u64>,
}

/// Reasons a quote cannot be executed. Each maps to a `409` response code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteError {
    /// Quote id is unknown, tampered with, or bound to another wallet.
    Invalid,
    /// Quote exists and matches but its expiry has passed.
    Expired,
    /// Quote was already consumed (replay protection).
    AlreadyUsed,
    /// Spot moved more than the configured maximum deviation.
    DeviationExceeded,
}

impl QuoteError {
    /// Stable API error code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            QuoteError::Invalid => QUOTE_INVALID,
            QuoteError::Expired | QuoteError::AlreadyUsed | QuoteError::DeviationExceeded => {
                QUOTE_EXPIRED
            }
        }
    }
}

/// Server-side quote store.
///
/// In production this is backed by the `quotes` table; the in-memory map keeps
/// the same semantics (single-use, wallet-bound, expiry-checked) so the
/// execution paths can be unit tested without a database.
pub struct QuoteStore {
    quotes: Mutex<HashMap<String, Quote>>,
    max_deviation_pct: f64,
    next_id: Mutex<u64>,
}

impl QuoteStore {
    pub fn new() -> Self {
        Self::with_max_deviation(DEFAULT_MAX_DEVIATION_PCT)
    }

    pub fn with_max_deviation(max_deviation_pct: f64) -> Self {
        Self {
            quotes: Mutex::new(HashMap::new()),
            max_deviation_pct,
            next_id: Mutex::new(1),
        }
    }

    /// Issue a new quote bound to the wallet, legs, size, side and expiry.
    pub fn issue(&self, req: QuoteRequest, premium: f64, spot: f64) -> Quote {
        let ttl = req.ttl_secs.unwrap_or(DEFAULT_QUOTE_TTL_SECS);
        let now = now_secs();
        let id = {
            let mut next = self.next_id.lock().expect("quote id lock poisoned");
            let id = format!("q_{:016x}", *next);
            *next += 1;
            id
        };
        let quote = Quote {
            id: id.clone(),
            wallet: req.wallet,
            legs: req.legs,
            premium,
            spot_at_issue: spot,
            expires_at: now + ttl,
            used_at: None,
        };
        self.quotes
            .lock()
            .expect("quote store lock poisoned")
            .insert(id, quote.clone());
        quote
    }

    /// Validate a quote for execution and atomically mark it consumed.
    ///
    /// Mirrors `UPDATE quotes SET used_at=? WHERE id=? AND used_at IS NULL`:
    /// the check and the consume happen under the same lock so a concurrent
    /// double-use can never both succeed.
    pub fn consume(
        &self,
        quote_id: &str,
        wallet: &str,
        spot: f64,
    ) -> Result<Quote, QuoteError> {
        let mut quotes = self.quotes.lock().expect("quote store lock poisoned");
        let quote = quotes.get_mut(quote_id).ok_or(QuoteError::Invalid)?;

        if quote.wallet != wallet {
            return Err(QuoteError::Invalid);
        }
        if quote.used_at.is_some() {
            return Err(QuoteError::AlreadyUsed);
        }
        let now = now_secs();
        if now > quote.expires_at {
            return Err(QuoteError::Expired);
        }
        if quote.spot_at_issue > 0.0 {
            let deviation = ((spot - quote.spot_at_issue) / quote.spot_at_issue).abs() * 100.0;
            if deviation > self.max_deviation_pct {
                return Err(QuoteError::DeviationExceeded);
            }
        }

        quote.used_at = Some(now);
        Ok(quote.clone())
    }

    /// Look up a quote without consuming it (used for previews/tests).
    pub fn get(&self, quote_id: &str) -> Option<Quote> {
        self.quotes
            .lock()
            .expect("quote store lock poisoned")
            .get(quote_id)
            .cloned()
    }
}

impl Default for QuoteStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Current server time in whole seconds since the Unix epoch.
///
/// Only server time is used; client clocks are never trusted.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg() -> QuoteLeg {
        QuoteLeg {
            series: "BTC-2024-12-31-C-50000".into(),
            side: Side::Buy,
            size: 1.0,
        }
    }

    fn request(wallet: &str) -> QuoteRequest {
        QuoteRequest {
            wallet: wallet.into(),
            legs: vec![leg()],
            ttl_secs: None,
        }
    }

    #[test]
    fn valid_execution_uses_quoted_premium() {
        let store = QuoteStore::new();
        let quote = store.issue(request("0xabc"), 1234.5, 50_000.0);
        let consumed = store.consume(&quote.id, "0xabc", 50_000.0).unwrap();
        assert_eq!(consumed.premium, 1234.5);
        assert!(consumed.used_at.is_some());
    }

    #[test]
    fn expired_quote_is_rejected() {
        let store = QuoteStore::new();
        let mut req = request("0xabc");
        req.ttl_secs = Some(0);
        let quote = store.issue(req, 10.0, 50_000.0);
        // Force expiry by rewinding the stored expiry.
        {
            let mut quotes = store.quotes.lock().unwrap();
            quotes.get_mut(&quote.id).unwrap().expires_at = now_secs() - 1;
        }
        assert_eq!(
            store.consume(&quote.id, "0xabc", 50_000.0),
            Err(QuoteError::Expired)
        );
        assert_eq!(QuoteError::Expired.code(), QUOTE_EXPIRED);
    }

    #[test]
    fn reuse_is_rejected() {
        let store = QuoteStore::new();
        let quote = store.issue(request("0xabc"), 10.0, 50_000.0);
        store.consume(&quote.id, "0xabc", 50_000.0).unwrap();
        assert_eq!(
            store.consume(&quote.id, "0xabc", 50_000.0),
            Err(QuoteError::AlreadyUsed)
        );
    }

    #[test]
    fn wallet_mismatch_is_invalid() {
        let store = QuoteStore::new();
        let quote = store.issue(request("0xabc"), 10.0, 50_000.0);
        assert_eq!(
            store.consume(&quote.id, "0xdef", 50_000.0),
            Err(QuoteError::Invalid)
        );
        assert_eq!(QuoteError::Invalid.code(), QUOTE_INVALID);
    }

    #[test]
    fn unknown_quote_is_invalid() {
        let store = QuoteStore::new();
        assert_eq!(
            store.consume("q_missing", "0xabc", 50_000.0),
            Err(QuoteError::Invalid)
        );
    }

    #[test]
    fn deviation_guard_rejects_large_moves() {
        let store = QuoteStore::with_max_deviation(0.5);
        let quote = store.issue(request("0xabc"), 10.0, 50_000.0);
        // 1% move exceeds the 0.5% guard.
        assert_eq!(
            store.consume(&quote.id, "0xabc", 50_500.0),
            Err(QuoteError::DeviationExceeded)
        );
    }

    #[test]
    fn concurrent_double_use_only_succeeds_once() {
        use std::sync::Arc;
        let store = Arc::new(QuoteStore::new());
        let quote = store.issue(request("0xabc"), 10.0, 50_000.0);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let id = quote.id.clone();
            handles.push(std::thread::spawn(move || {
                store.consume(&id, "0xabc", 50_000.0).is_ok()
            }));
        }
        let successes = handles
            .into_iter()
            .filter(|h| h.join().unwrap())
            .count();
        assert_eq!(successes, 1);
    }
}
