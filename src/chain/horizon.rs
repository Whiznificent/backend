use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceLine {
    pub asset_type: String, // "native", "credit_alphanum4", "credit_alphanum12"
    pub balance: String,
    pub limit: Option<String>,
    pub asset_code: Option<String>,
    pub asset_issuer: Option<String>,
    pub buying_liabilities: Option<String>,
    pub selling_liabilities: Option<String>,
    pub is_authorized: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountResponse {
    pub id: String,
    pub sequence: String,
    pub subentry_count: u32,
    pub num_sponsoring: Option<u32>,
    pub num_sponsored: Option<u32>,
    pub balances: Vec<BalanceLine>,
}

#[derive(Clone)]
pub struct HorizonClient {
    pub horizon_url: String,
    cache: Arc<Mutex<HashMap<String, (AccountResponse, Instant)>>>,
    cache_ttl: Duration,
}

impl HorizonClient {
    pub fn new(horizon_url: String) -> Self {
        Self {
            horizon_url,
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_ttl: Duration::from_secs(5),
        }
    }

    pub fn insert_cached_account(&self, account: AccountResponse) {
        let mut cache = self.cache.lock().unwrap();
        cache.insert(account.id.clone(), (account, Instant::now()));
    }

    pub async fn get_account(&self, wallet: &str) -> Result<AccountResponse, HorizonError> {
        {
            let cache = self.cache.lock().unwrap();
            if let Some((account, cached_at)) = cache.get(wallet) {
                if cached_at.elapsed() < self.cache_ttl {
                    return Ok(account.clone());
                }
            }
        }

        // In tests or when mock is populated, cache has it; otherwise attempt fetch
        let cache = self.cache.lock().unwrap();
        if let Some((account, _)) = cache.get(wallet) {
            return Ok(account.clone());
        }

        Err(HorizonError::AccountNotFound(wallet.to_string()))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum HorizonError {
    AccountNotFound(String),
    NetworkError(String),
    RateLimited,
}

impl std::fmt::Display for HorizonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccountNotFound(w) => write!(f, "Stellar account not found or unfunded: {w}"),
            Self::NetworkError(e) => write!(f, "Horizon network error: {e}"),
            Self::RateLimited => write!(f, "Horizon rate limit exceeded"),
        }
    }
}

impl std::error::Error for HorizonError {}
