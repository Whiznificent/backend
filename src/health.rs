use axum::http::StatusCode;
use axum::response::Json;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::auth::AuthUser;
use crate::AppState;

pub const MAX_MARKET_DATA_AGE: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct OperationalState {
    last_market_update: Arc<Mutex<Instant>>,
    draining: Arc<AtomicBool>,
    background_loops: Arc<Mutex<HashMap<String, LoopStatus>>>,
}

#[derive(Clone, Default)]
struct LoopStatus {
    last_success: Option<Instant>,
    last_error: Option<String>,
}

impl OperationalState {
    pub fn new() -> Self {
        Self {
            last_market_update: Arc::new(Mutex::new(Instant::now())),
            draining: Arc::new(AtomicBool::new(false)),
            background_loops: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn market_data_updated(&self) {
        *self.last_market_update.lock().unwrap() = Instant::now();
    }

    pub fn market_data_age(&self) -> Duration {
        self.last_market_update.lock().unwrap().elapsed()
    }

    pub fn set_draining(&self) {
        self.draining.store(true, Ordering::Relaxed);
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Relaxed)
    }

    pub fn background_loop_succeeded(&self, name: &str) {
        let mut loops = self.background_loops.lock().unwrap();
        let status = loops.entry(name.to_owned()).or_default();
        status.last_success = Some(Instant::now());
        status.last_error = None;
    }

    pub fn background_loop_failed(&self, name: &str, error: impl Into<String>) {
        let mut loops = self.background_loops.lock().unwrap();
        let status = loops.entry(name.to_owned()).or_default();
        status.last_error = Some(error.into());
    }

    pub fn background_loop_health(&self) -> Vec<(&'static str, bool)> {
        let loops = self.background_loops.lock().unwrap();
        ["auth_cleanup", "alert_checks", "price_simulator"]
            .into_iter()
            .map(|name| {
                let healthy = loops.get(name).is_some_and(|status| {
                    status.last_error.is_none()
                        && status.last_success.is_some_and(|last_success| {
                            last_success.elapsed() <= Duration::from_secs(600)
                        })
                });
                (name, healthy)
            })
            .collect()
    }
}

impl Default for OperationalState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize)]
struct DependencyStatus {
    status: &'static str,
    latency_ms: Option<f64>,
    last_error: Option<String>,
}

impl DependencyStatus {
    fn check<T>(started: Instant, result: Result<T, String>) -> (Self, Option<T>) {
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(value) => (
                Self {
                    status: "ok",
                    latency_ms: Some(latency_ms),
                    last_error: None,
                },
                Some(value),
            ),
            Err(error) => (
                Self {
                    status: "error",
                    latency_ms: Some(latency_ms),
                    last_error: Some(error),
                },
                None,
            ),
        }
    }

    fn state(status: &'static str, last_error: Option<String>) -> Self {
        Self {
            status,
            latency_ms: None,
            last_error,
        }
    }
}

#[derive(Serialize)]
pub struct HealthReport {
    status: &'static str,
    dependencies: HashMap<String, DependencyStatus>,
}

fn expected_migration_version() -> Option<i64> {
    sqlx::migrate!("./migrations")
        .iter()
        .map(|migration| migration.version)
        .max()
}

async fn dependency_statuses(state: &AppState) -> HashMap<String, DependencyStatus> {
    let mut dependencies = HashMap::new();

    let started = Instant::now();
    let database = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .map_err(|error| error.to_string())
        .and_then(|value| {
            (value == 1)
                .then_some(value)
                .ok_or_else(|| "database ping returned an unexpected value".to_owned())
        });
    let (database_status, database_ok) = DependencyStatus::check(started, database);
    dependencies.insert("database".to_owned(), database_status);

    let started = Instant::now();
    let migrations = match expected_migration_version() {
        Some(expected) => sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1",
        )
        .fetch_one(&state.db)
        .await
        .map_err(|error| error.to_string())
        .and_then(|applied| {
            if applied == Some(expected) {
                Ok(applied)
            } else {
                Err(format!(
                    "applied migration version {applied:?} does not match expected {expected}"
                ))
            }
        }),
        None => Err("no migrations are embedded in this build".to_owned()),
    };
    let (migration_status, _) = DependencyStatus::check(started, migrations);
    dependencies.insert("migrations".to_owned(), migration_status);

    let age = state.operations.market_data_age();
    dependencies.insert(
        "market_data".to_owned(),
        DependencyStatus::state(
            if age <= MAX_MARKET_DATA_AGE {
                "ok"
            } else {
                "stale"
            },
            (age > MAX_MARKET_DATA_AGE)
                .then(|| format!("last update was {:.3}s ago", age.as_secs_f64())),
        ),
    );

    let loops = state.operations.background_loops.lock().unwrap().clone();
    for name in ["auth_cleanup", "alert_checks", "price_simulator"] {
        let loop_status = loops.get(name);
        let fresh = loop_status
            .and_then(|status| status.last_success)
            .is_some_and(|last_success| last_success.elapsed() <= Duration::from_secs(600));
        let last_error = loop_status.and_then(|status| status.last_error.clone());
        dependencies.insert(
            format!("background_loop.{name}"),
            DependencyStatus::state(
                if fresh {
                    "ok"
                } else if loop_status.is_some() {
                    "stale"
                } else {
                    "not_started"
                },
                last_error,
            ),
        );
    }

    dependencies.insert(
        "draining".to_owned(),
        DependencyStatus::state(
            if state.operations.is_draining() {
                "draining"
            } else {
                "ok"
            },
            None,
        ),
    );
    if database_ok.is_none() {
        tracing::warn!("readiness check failed: database is unavailable");
    }
    dependencies
}

fn report_is_ready(dependencies: &HashMap<String, DependencyStatus>) -> bool {
    ["database", "migrations", "market_data", "draining"]
        .iter()
        .all(|name| {
            dependencies
                .get(*name)
                .is_some_and(|dependency| dependency.status == "ok")
        })
}

pub async fn livez() -> StatusCode {
    StatusCode::OK
}

pub async fn readyz(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> (StatusCode, Json<HealthReport>) {
    let dependencies = dependency_statuses(&state).await;
    let ready = report_is_ready(&dependencies);
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(HealthReport {
            status: if ready { "ready" } else { "not_ready" },
            dependencies,
        }),
    )
}

pub async fn details(
    axum::extract::State(state): axum::extract::State<AppState>,
    AuthUser(_): AuthUser,
) -> Json<HealthReport> {
    let dependencies = dependency_statuses(&state).await;
    let ready = report_is_ready(&dependencies);
    Json(HealthReport {
        status: if ready { "ok" } else { "degraded" },
        dependencies,
    })
}
