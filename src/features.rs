use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
    time::Duration,
};

use crate::{
    error::{db_error, AppError},
    AppState,
};

#[derive(Clone, Debug)]
struct Flag {
    enabled: bool,
    rollout_percent: f64,
    wallets: HashSet<String>,
}

#[derive(Clone)]
pub struct FeatureFlagCache {
    environment: Arc<str>,
    flags: Arc<RwLock<HashMap<String, Flag>>>,
}

impl FeatureFlagCache {
    pub fn new(environment: impl Into<Arc<str>>) -> Self {
        Self {
            environment: environment.into(),
            flags: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    pub async fn refresh(&self, db: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
        let mut tx = db.begin().await?;
        let rows: Vec<(String, bool, f64)> = sqlx::query_as(
            "SELECT name, enabled, rollout_percent FROM feature_flags WHERE environment = ?",
        )
        .bind(self.environment())
        .fetch_all(&mut *tx)
        .await?;

        let wallets: Vec<(String, String)> = sqlx::query_as(
            "SELECT flag_name, wallet_address FROM feature_flag_wallets WHERE environment = ?",
        )
        .bind(self.environment())
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        let mut flags = rows
            .into_iter()
            .map(|(name, enabled, rollout_percent)| {
                (
                    name,
                    Flag {
                        enabled,
                        rollout_percent,
                        wallets: HashSet::new(),
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        for (name, wallet) in wallets {
            if let Some(flag) = flags.get_mut(&name) {
                flag.wallets.insert(wallet);
            }
        }

        *self
            .flags
            .write()
            .expect("feature flag cache lock poisoned") = flags;
        Ok(())
    }

    pub fn is_enabled(&self, name: &str, wallet: Option<&str>) -> bool {
        let flags = self.flags.read().expect("feature flag cache lock poisoned");
        let Some(flag) = flags.get(name) else {
            return false;
        };
        if !flag.enabled {
            return false;
        }
        if wallet.is_some_and(|wallet| flag.wallets.contains(wallet)) {
            return true;
        }
        if flag.rollout_percent <= 0.0 {
            return false;
        }
        if flag.rollout_percent >= 100.0 {
            return true;
        }

        let subject = wallet.unwrap_or("anonymous");
        stable_bucket(name, subject) < flag.rollout_percent
    }

    pub async fn refresh_loop(self, db: sqlx::SqlitePool) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = self.refresh(&db).await {
                tracing::error!(error = %error, "refresh feature flag cache failed");
            }
        }
    }
}

fn stable_bucket(flag: &str, subject: &str) -> f64 {
    // FNV-1a gives each wallet/flag pair a stable bucket across API instances.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in flag.bytes().chain([0xff]).chain(subject.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % 10_000) as f64 / 100.0
}

#[derive(Serialize)]
pub struct FeatureResponse {
    pub environment: String,
    pub features: HashMap<String, bool>,
}

pub async fn get_features(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<FeatureResponse>, AppError> {
    let wallet = if let Some(token) = bearer_token(&headers) {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT wallet_address FROM sessions
             WHERE token = ? AND expires_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(token)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| db_error("look up feature evaluation session", e))?;
        row.map(|(wallet,)| wallet)
    } else {
        None
    };

    let names = state
        .feature_flags
        .flags
        .read()
        .map_err(|_| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "feature cache unavailable",
            )
        })?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let snapshot = names
        .into_iter()
        .map(|name| {
            let enabled = state.feature_flags.is_enabled(&name, wallet.as_deref());
            (name, enabled)
        })
        .collect();

    Ok(Json(FeatureResponse {
        environment: state.feature_flags.environment().to_string(),
        features: snapshot,
    }))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with_flag(enabled: bool, rollout_percent: f64, wallets: &[&str]) -> FeatureFlagCache {
        let cache = FeatureFlagCache::new("test");
        let flags = HashMap::from([(
            "example".to_string(),
            Flag {
                enabled,
                rollout_percent,
                wallets: wallets.iter().map(|wallet| wallet.to_string()).collect(),
            },
        )]);
        *cache.flags.write().unwrap() = flags;
        cache
    }

    #[test]
    fn disabled_flag_cannot_be_enabled_by_rollout_or_allowlist() {
        let cache = cache_with_flag(false, 100.0, &["GALLOW"]);
        assert!(!cache.is_enabled("example", Some("GALLOW")));
        assert!(!cache.is_enabled("example", Some("GOTHER")));
    }

    #[test]
    fn allowlisted_wallet_is_enabled_and_unknown_flag_is_disabled() {
        let cache = cache_with_flag(true, 0.0, &["GALLOW"]);
        assert!(cache.is_enabled("example", Some("GALLOW")));
        assert!(!cache.is_enabled("example", Some("GOTHER")));
        assert!(!cache.is_enabled("missing", Some("GALLOW")));
    }

    #[test]
    fn percentage_rollout_is_stable_and_respects_boundaries() {
        let cache = cache_with_flag(true, 100.0, &[]);
        assert!(cache.is_enabled("example", Some("GWALLET")));
        let first = stable_bucket("example", "GWALLET") < 43.0;
        let second = cache_with_flag(true, 43.0, &[]).is_enabled("example", Some("GWALLET"));
        assert_eq!(first, second);
        assert!(!cache_with_flag(true, 0.0, &[]).is_enabled("example", Some("GWALLET")));
    }

    #[tokio::test]
    async fn refresh_loads_only_the_selected_environment_and_allowlist() {
        let db_path =
            std::env::temp_dir().join(format!("zenith-features-test-{}.db", uuid::Uuid::new_v4()));
        let db = crate::db::init_pool(&format!("sqlite://{}", db_path.display())).await;
        for environment in ["staging", "production"] {
            sqlx::query(
                "INSERT INTO feature_flags (environment, name, enabled, rollout_percent)
                 VALUES (?, 'checkout', 1, 0)",
            )
            .bind(environment)
            .execute(&db)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO feature_flag_wallets (environment, flag_name, wallet_address)
             VALUES ('staging', 'checkout', 'GWALLET')",
        )
        .execute(&db)
        .await
        .unwrap();

        let cache = FeatureFlagCache::new("staging");
        cache.refresh(&db).await.unwrap();
        assert!(cache.is_enabled("checkout", Some("GWALLET")));
        assert!(!cache.is_enabled("checkout", Some("GOTHER")));
        assert!(!FeatureFlagCache::new("production").is_enabled("checkout", Some("GWALLET")));

        db.close().await;
        let _ = std::fs::remove_file(db_path);
    }
}
