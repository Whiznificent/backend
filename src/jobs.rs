use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::{
    error::{db_error, AppError},
    AppState,
};

const BASE_BACKOFF_SECS: i64 = 2;
const MAX_BACKOFF_SECS: i64 = 300;

type ExportPosition = (
    String,
    String,
    String,
    f64,
    f64,
    String,
    String,
    f64,
    String,
);

type JobRecordRow = (
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

#[derive(Clone, Debug)]
pub struct ClaimedJob {
    pub id: String,
    pub kind: String,
    pub payload: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub timeout_secs: i64,
}

#[derive(Serialize)]
pub struct JobRecord {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub scheduled_at: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub timeout_secs: i64,
    pub unique_key: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

pub async fn enqueue(
    db: &sqlx::SqlitePool,
    kind: &str,
    payload: &serde_json::Value,
    unique_key: Option<&str>,
    max_attempts: i64,
    timeout_secs: i64,
) -> Result<String, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT OR IGNORE INTO jobs
           (id, kind, payload, unique_key, max_attempts, timeout_secs)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(kind)
    .bind(payload.to_string())
    .bind(unique_key)
    .bind(max_attempts.max(1))
    .bind(timeout_secs.max(1))
    .execute(db)
    .await?;
    if let Some(unique_key) = unique_key {
        let active_id: String = sqlx::query_scalar(
            "SELECT id FROM jobs WHERE unique_key = ? AND status IN ('queued', 'running')",
        )
        .bind(unique_key)
        .fetch_one(db)
        .await?;
        Ok(active_id)
    } else {
        Ok(id)
    }
}

fn periodic_interval(kind: &str) -> Option<i64> {
    match kind {
        "auth_cleanup" => Some(300),
        "check_alerts" => Some(10),
        "price_tick" => Some(2),
        "snapshot" => Some(60),
        "retention" | "export" => Some(86_400),
        _ => None,
    }
}

pub async fn ensure_periodic_jobs(db: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    for kind in [
        "auth_cleanup",
        "check_alerts",
        "price_tick",
        "snapshot",
        "retention",
        "export",
    ] {
        enqueue(
            db,
            kind,
            &serde_json::Value::Null,
            Some(&format!("periodic:{kind}")),
            5,
            match kind {
                "export" | "retention" => 300,
                "snapshot" => 30,
                "price_tick" => 10,
                "check_alerts" => 30,
                _ => 60,
            },
        )
        .await?;
    }
    Ok(())
}

async fn recover_expired_leases(db: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    let expired: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, kind, attempts, max_attempts FROM jobs
         WHERE status = 'running' AND lease_until < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .fetch_all(db)
    .await?;
    sqlx::query(
        "UPDATE job_attempts
         SET ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             outcome = 'lease_expired', error = 'worker lease expired'
         WHERE outcome = 'running' AND job_id IN (
           SELECT id FROM jobs
           WHERE status = 'running' AND lease_until < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
    )
    .execute(db)
    .await?;
    sqlx::query(
        "UPDATE jobs SET
           status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
           scheduled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
           lease_owner = NULL, lease_until = NULL,
           last_error = 'worker lease expired',
           completed_at = CASE WHEN attempts >= max_attempts THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now') ELSE NULL END,
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE status = 'running' AND lease_until < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .execute(db)
    .await?;
    for (_, kind, attempts, max_attempts) in expired {
        if attempts >= max_attempts {
            schedule_periodic_successor(db, &kind).await?;
        }
    }
    Ok(())
}

async fn claim_job(
    db: &sqlx::SqlitePool,
    worker_id: &str,
) -> Result<Option<ClaimedJob>, sqlx::Error> {
    recover_expired_leases(db).await?;
    let mut tx = db.begin().await?;
    let row: Option<(String, String, String, i64, i64, i64)> = sqlx::query_as(
        "UPDATE jobs SET
           status = 'running',
           attempts = attempts + 1,
           lease_owner = ?,
           lease_until = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+' || (timeout_secs + 30) || ' seconds'),
           started_at = COALESCE(started_at, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = (
           SELECT id FROM jobs
           WHERE status = 'queued' AND scheduled_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
           ORDER BY scheduled_at, created_at LIMIT 1
         ) AND status = 'queued'
         RETURNING id, kind, payload, attempts, max_attempts, timeout_secs",
    )
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, kind, payload, attempts, max_attempts, timeout_secs)) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    sqlx::query(
        "INSERT INTO job_attempts (id, job_id, attempt_number, worker_id, outcome)
         VALUES (?, ?, ?, ?, 'running')",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&id)
    .bind(attempts)
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(ClaimedJob {
        id,
        kind,
        payload,
        attempts,
        max_attempts,
        timeout_secs,
    }))
}

async fn complete_success(
    db: &sqlx::SqlitePool,
    worker_id: &str,
    job: &ClaimedJob,
    result: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    let updated = sqlx::query(
        "UPDATE jobs SET status = 'succeeded', result_json = ?, last_error = NULL,
           lease_owner = NULL, lease_until = NULL, completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ? AND status = 'running' AND lease_owner = ?",
    )
    .bind(result.to_string())
    .bind(&job.id)
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 1 {
        sqlx::query(
            "UPDATE job_attempts SET outcome = 'succeeded',
               ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_id = ? AND attempt_number = ? AND worker_id = ?",
        )
        .bind(&job.id)
        .bind(job.attempts)
        .bind(worker_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    if updated.rows_affected() == 1 {
        schedule_periodic_successor(db, &job.kind).await?;
    }
    Ok(())
}

async fn complete_failure(
    db: &sqlx::SqlitePool,
    worker_id: &str,
    job: &ClaimedJob,
    error: &str,
    timeout: bool,
) -> Result<(), sqlx::Error> {
    let dead = job.attempts >= job.max_attempts;
    let backoff_secs = (BASE_BACKOFF_SECS * 2_i64.pow((job.attempts - 1).clamp(0, 20) as u32))
        .min(MAX_BACKOFF_SECS);
    let status = if dead { "dead" } else { "queued" };
    let outcome = if timeout {
        "timeout"
    } else if dead {
        "dead"
    } else {
        "retry"
    };
    let mut tx = db.begin().await?;
    let updated = sqlx::query(
        "UPDATE jobs SET status = ?, scheduled_at = CASE WHEN ? = 'queued'
             THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+' || ? || ' seconds')
             ELSE scheduled_at END,
           lease_owner = NULL, lease_until = NULL, last_error = ?,
           completed_at = CASE WHEN ? = 'dead' THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now') ELSE NULL END,
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ? AND status = 'running' AND lease_owner = ?",
    )
    .bind(status)
    .bind(status)
    .bind(backoff_secs)
    .bind(error)
    .bind(status)
    .bind(&job.id)
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 1 {
        sqlx::query(
            "UPDATE job_attempts SET outcome = ?, error = ?,
               ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE job_id = ? AND attempt_number = ? AND worker_id = ?",
        )
        .bind(outcome)
        .bind(error)
        .bind(&job.id)
        .bind(job.attempts)
        .bind(worker_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    if dead && updated.rows_affected() == 1 {
        schedule_periodic_successor(db, &job.kind).await?;
    }
    Ok(())
}

async fn schedule_periodic_successor(db: &sqlx::SqlitePool, kind: &str) -> Result<(), sqlx::Error> {
    if periodic_interval(kind).is_some() {
        let delay = periodic_interval(kind).expect("periodic task checked");
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT OR IGNORE INTO jobs
               (id, kind, scheduled_at, unique_key, max_attempts, timeout_secs)
             VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+' || ? || ' seconds'),
                     ?, 5, ?)",
        )
        .bind(id)
        .bind(kind)
        .bind(delay)
        .bind(format!("periodic:{kind}"))
        .bind(match kind {
            "export" | "retention" => 300,
            "snapshot" => 30,
            "price_tick" => 10,
            "check_alerts" => 30,
            _ => 60,
        })
        .execute(db)
        .await?;
    }
    Ok(())
}

async fn execute_job(state: &AppState, job: &ClaimedJob) -> Result<serde_json::Value, String> {
    match job.kind.as_str() {
        "auth_cleanup" => {
            let (nonces, sessions) = crate::auth::sweep_expired(&state.db)
                .await
                .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({"expired_nonces": nonces, "expired_sessions": sessions}))
        }
        "check_alerts" => {
            let triggered = crate::alerts::check_once(state)
                .await
                .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({"triggered": triggered}))
        }
        "price_tick" => {
            let payload = crate::prices::tick_and_persist(state)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::from_str(&payload).map_err(|error| error.to_string())
        }
        "snapshot" => {
            let payload = serde_json::json!({
                "prices": state.spot_prices.lock().map_err(|_| "price cache lock poisoned")?.clone(),
                "vols": state.vol_surface.lock().map_err(|_| "vol cache lock poisoned")?.clone()
            });
            sqlx::query("INSERT INTO market_snapshots (id, payload) VALUES (?, ?)")
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(payload.to_string())
                .execute(&state.db)
                .await
                .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({"captured": true}))
        }
        "retention" => {
            let retention_days = std::env::var("ZENITH_RETENTION_DAYS")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(90)
                .clamp(1, 3650);
            let snapshots = sqlx::query(
                "DELETE FROM market_snapshots
                 WHERE captured_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-' || ? || ' days')",
            )
            .bind(retention_days)
            .execute(&state.db)
            .await
            .map_err(|error| error.to_string())?;
            let artifacts = sqlx::query(
                "DELETE FROM job_artifacts
                 WHERE created_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 days')",
            )
            .execute(&state.db)
            .await
            .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({
                "snapshots_deleted": snapshots.rows_affected(),
                "artifacts_deleted": artifacts.rows_affected()
            }))
        }
        "export" => {
            let accounts: Vec<(String, f64, f64, String)> = sqlx::query_as(
                "SELECT wallet_address, balance, collateral_locked, created_at
                 FROM accounts ORDER BY wallet_address",
            )
            .fetch_all(&state.db)
            .await
            .map_err(|error| error.to_string())?;
            let positions: Vec<ExportPosition> = sqlx::query_as(
                "SELECT id, wallet_address, underlying, strike, contracts,
                        option_type, position_type, entry_premium, status
                 FROM positions ORDER BY opened_at",
            )
            .fetch_all(&state.db)
            .await
            .map_err(|error| error.to_string())?;
            let alerts: Vec<(String, String, String, String, f64, bool)> = sqlx::query_as(
                "SELECT id, wallet_address, underlying, condition, target_price, triggered
                 FROM alerts ORDER BY created_at",
            )
            .fetch_all(&state.db)
            .await
            .map_err(|error| error.to_string())?;
            let export = serde_json::json!({
                "accounts": accounts.into_iter().map(|(wallet_address, balance, collateral_locked, created_at)| {
                    serde_json::json!({"wallet_address": wallet_address, "balance": balance,
                        "collateral_locked": collateral_locked, "created_at": created_at})
                }).collect::<Vec<_>>(),
                "positions": positions.into_iter().map(|(id, wallet_address, underlying, strike, contracts,
                    option_type, position_type, entry_premium, status)| {
                    serde_json::json!({"id": id, "wallet_address": wallet_address, "underlying": underlying,
                        "strike": strike, "contracts": contracts, "option_type": option_type,
                        "position_type": position_type, "entry_premium": entry_premium, "status": status})
                }).collect::<Vec<_>>(),
                "alerts": alerts.into_iter().map(|(id, wallet_address, underlying, condition, target_price, triggered)| {
                    serde_json::json!({"id": id, "wallet_address": wallet_address, "underlying": underlying,
                        "condition": condition, "target_price": target_price, "triggered": triggered})
                }).collect::<Vec<_>>()
            });
            sqlx::query(
                "INSERT INTO job_artifacts (job_id, content_json) VALUES (?, ?)
                 ON CONFLICT(job_id) DO UPDATE SET content_json = excluded.content_json,
                   created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            )
            .bind(&job.id)
            .bind(export.to_string())
            .execute(&state.db)
            .await
            .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({"artifact_job_id": job.id}))
        }
        "reconcile_accounts" => reconcile_accounts(&state.db, &job.id).await,
        #[cfg(test)]
        "test_sleep" => {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(serde_json::Value::Null)
        }
        kind => Err(format!("unsupported job kind: {kind}")),
    }
}

async fn reconcile_accounts(
    db: &sqlx::SqlitePool,
    job_id: &str,
) -> Result<serde_json::Value, String> {
    let expected: Vec<(String, f64)> = sqlx::query_as(
        "SELECT wallet_address, COALESCE(SUM(collateral), 0.0)
         FROM positions WHERE status = 'open' GROUP BY wallet_address",
    )
    .fetch_all(db)
    .await
    .map_err(|error| error.to_string())?;
    let actual: Vec<(String, f64)> =
        sqlx::query_as("SELECT wallet_address, collateral_locked FROM accounts")
            .fetch_all(db)
            .await
            .map_err(|error| error.to_string())?;
    let expected = expected
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    let differences = actual
        .into_iter()
        .filter_map(|(wallet, locked)| {
            let calculated = expected.get(&wallet).copied().unwrap_or(0.0);
            ((locked - calculated).abs() > 0.000_001).then_some(serde_json::json!({
                "wallet_address": wallet,
                "stored_collateral_locked": locked,
                "calculated_collateral_locked": calculated
            }))
        })
        .collect::<Vec<_>>();
    let result = serde_json::json!({"differences": differences});
    sqlx::query(
        "INSERT INTO reconciliation_runs (id, job_id, differences, result_json)
         VALUES (?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(job_id)
    .bind(differences.len() as i64)
    .bind(result.to_string())
    .execute(db)
    .await
    .map_err(|error| error.to_string())?;
    Ok(result)
}

pub async fn run_one(state: &AppState, worker_id: &str) -> Result<bool, sqlx::Error> {
    let Some(job) = claim_job(&state.db, worker_id).await? else {
        return Ok(false);
    };
    let result = tokio::time::timeout(
        Duration::from_secs(job.timeout_secs as u64),
        execute_job(state, &job),
    )
    .await;
    match result {
        Ok(Ok(value)) => complete_success(&state.db, worker_id, &job, &value).await?,
        Ok(Err(error)) => {
            tracing::warn!(job_id = %job.id, kind = %job.kind, error = %error, "job failed");
            complete_failure(&state.db, worker_id, &job, &error, false).await?;
        }
        Err(_) => {
            let error = format!("job exceeded {} second timeout", job.timeout_secs);
            tracing::warn!(job_id = %job.id, kind = %job.kind, "job timed out");
            complete_failure(&state.db, worker_id, &job, &error, true).await?;
        }
    }
    Ok(true)
}

pub async fn run_loop(state: AppState) {
    let worker_id = format!("{}:{}", uuid::Uuid::new_v4(), std::process::id());
    if let Err(error) = ensure_periodic_jobs(&state.db).await {
        tracing::error!(error = %error, "seed periodic jobs failed");
    }
    let mut seed_interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = seed_interval.tick() => {
                if let Err(error) = ensure_periodic_jobs(&state.db).await {
                    tracing::error!(error = %error, "refresh periodic jobs failed");
                }
            }
            result = run_one(&state, &worker_id) => {
                match result {
                    Ok(true) => {}
                    Ok(false) => tokio::time::sleep(Duration::from_millis(500)).await,
                    Err(error) => {
                        tracing::error!(error = %error, "job worker iteration failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }
}

pub async fn list_jobs(db: &sqlx::SqlitePool) -> Result<Vec<JobRecord>, sqlx::Error> {
    let rows: Vec<JobRecordRow> = sqlx::query_as(
        "SELECT id, kind, status, scheduled_at, attempts, max_attempts, timeout_secs,
                unique_key, last_error, created_at, completed_at
         FROM jobs ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                kind,
                status,
                scheduled_at,
                attempts,
                max_attempts,
                timeout_secs,
                unique_key,
                last_error,
                created_at,
                completed_at,
            )| JobRecord {
                id,
                kind,
                status,
                scheduled_at,
                attempts,
                max_attempts,
                timeout_secs,
                unique_key,
                last_error,
                created_at,
                completed_at,
            },
        )
        .collect())
}

#[derive(Serialize)]
pub struct JobMetrics {
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub dead: i64,
    pub attempts: i64,
    pub failed_attempts: i64,
}

pub async fn metrics(db: &sqlx::SqlitePool) -> Result<JobMetrics, sqlx::Error> {
    let (queued, running, succeeded, dead): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
           COALESCE(SUM(status = 'queued'), 0), COALESCE(SUM(status = 'running'), 0),
           COALESCE(SUM(status = 'succeeded'), 0), COALESCE(SUM(status = 'dead'), 0)
         FROM jobs",
    )
    .fetch_one(db)
    .await?;
    let (attempts, failed_attempts): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(outcome IN ('retry', 'dead', 'timeout', 'lease_expired')), 0)
         FROM job_attempts",
    )
    .fetch_one(db)
    .await?;
    Ok(JobMetrics {
        queued,
        running,
        succeeded,
        dead,
        attempts,
        failed_attempts,
    })
}

pub async fn get_admin_jobs(
    State(state): State<AppState>,
    crate::auth::AuthUser(wallet): crate::auth::AuthUser,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<JobRecord>>, AppError> {
    crate::admin::require_admin_access(
        &state,
        &wallet,
        &headers,
        crate::admin::AdminRole::Viewer,
        false,
    )
    .await?;
    list_jobs(&state.db)
        .await
        .map(Json)
        .map_err(|e| db_error("list background jobs", e))
}

pub async fn get_admin_job_metrics(
    State(state): State<AppState>,
    crate::auth::AuthUser(wallet): crate::auth::AuthUser,
    headers: axum::http::HeaderMap,
) -> Result<Json<JobMetrics>, AppError> {
    crate::admin::require_admin_access(
        &state,
        &wallet,
        &headers,
        crate::admin::AdminRole::Viewer,
        false,
    )
    .await?;
    metrics(&state.db)
        .await
        .map(Json)
        .map_err(|e| db_error("load background job metrics", e))
}

pub async fn retry_admin_job(
    State(state): State<AppState>,
    crate::auth::AuthUser(wallet): crate::auth::AuthUser,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    crate::admin::require_admin_access(
        &state,
        &wallet,
        &headers,
        crate::admin::AdminRole::Operator,
        true,
    )
    .await?;
    let unique_key: Option<String> =
        sqlx::query_scalar("SELECT unique_key FROM jobs WHERE id = ? AND status = 'dead'")
            .bind(&id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| db_error("look up dead-letter job", e))?
            .flatten();
    if let Some(key) = &unique_key {
        let active: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE unique_key = ? AND status IN ('queued', 'running'))",
        )
        .bind(key)
        .fetch_one(&state.db)
        .await
        .map_err(|e| db_error("check active unique job", e))?;
        if active == 1 {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "an active job already uses this uniqueness key",
            ));
        }
    }
    let result = sqlx::query(
        "UPDATE jobs SET status = 'queued', attempts = 0, scheduled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
           lease_owner = NULL, lease_until = NULL, last_error = NULL, result_json = NULL,
           completed_at = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ? AND status = 'dead'",
    )
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(|e| {
        if e.as_database_error().is_some_and(|db_error| db_error.is_unique_violation()) {
            AppError::new(
                StatusCode::CONFLICT,
                "an active job already uses this uniqueness key",
            )
        } else {
            db_error("retry dead-letter job", e)
        }
    })?;
    if result.rows_affected() == 0 {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "job not found or is not dead-lettered",
        ));
    }

    Ok(StatusCode::ACCEPTED)
}

#[derive(Serialize)]
pub struct JobArtifact {
    pub job_id: String,
    pub created_at: String,
    pub content: serde_json::Value,
}

pub async fn get_admin_job_artifact(
    State(state): State<AppState>,
    crate::auth::AuthUser(wallet): crate::auth::AuthUser,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<JobArtifact>, AppError> {
    crate::admin::require_admin_access(
        &state,
        &wallet,
        &headers,
        crate::admin::AdminRole::Viewer,
        false,
    )
    .await?;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT created_at, content_json FROM job_artifacts WHERE job_id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| db_error("load job export artifact", e))?;
    let (created_at, content_json) =
        row.ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "job artifact not found"))?;
    let content = serde_json::from_str(&content_json).map_err(|error| {
        tracing::error!(error = %error, job_id = %id, "stored export artifact is invalid JSON");
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "stored export artifact is invalid",
        )
    })?;
    Ok(Json(JobArtifact {
        job_id: id,
        created_at,
        content,
    }))
}

#[derive(Deserialize)]
pub struct EnqueueJobRequest {
    pub kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub unique_key: Option<String>,
}

#[derive(Serialize)]
pub struct EnqueueJobResponse {
    pub id: String,
}

pub async fn post_admin_job(
    State(state): State<AppState>,
    crate::auth::AuthUser(wallet): crate::auth::AuthUser,
    headers: axum::http::HeaderMap,
    crate::error::AppJson(request): crate::error::AppJson<EnqueueJobRequest>,
) -> Result<(StatusCode, Json<EnqueueJobResponse>), AppError> {
    crate::admin::require_admin_access(
        &state,
        &wallet,
        &headers,
        crate::admin::AdminRole::Operator,
        true,
    )
    .await?;
    if ![
        "auth_cleanup",
        "check_alerts",
        "price_tick",
        "snapshot",
        "retention",
        "export",
        "reconcile_accounts",
    ]
    .contains(&request.kind.as_str())
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "unsupported job kind",
        ));
    }
    let id = enqueue(
        &state.db,
        &request.kind,
        &request.payload,
        request.unique_key.as_deref(),
        5,
        60,
    )
    .await
    .map_err(|e| db_error("enqueue admin job", e))?;
    Ok((StatusCode::ACCEPTED, Json(EnqueueJobResponse { id })))
}

#[derive(Serialize)]
pub struct ReconciliationRun {
    pub id: String,
    pub job_id: String,
    pub created_at: String,
    pub differences: i64,
    pub result: serde_json::Value,
}

pub async fn get_reconciliation_runs(
    State(state): State<AppState>,
    crate::auth::AuthUser(wallet): crate::auth::AuthUser,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<ReconciliationRun>>, AppError> {
    crate::admin::require_admin_access(
        &state,
        &wallet,
        &headers,
        crate::admin::AdminRole::Viewer,
        false,
    )
    .await?;
    let rows: Vec<(String, String, String, i64, String)> = sqlx::query_as(
        "SELECT id, job_id, created_at, differences, result_json
         FROM reconciliation_runs ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| db_error("list reconciliation runs", e))?;
    let mut result = Vec::with_capacity(rows.len());
    for (id, job_id, created_at, differences, result_json) in rows {
        let result_value = serde_json::from_str(&result_json)
            .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        result.push(ReconciliationRun {
            id,
            job_id,
            created_at,
            differences,
            result: result_value,
        });
    }
    Ok(Json(result))
}

pub async fn post_reconciliation(
    State(state): State<AppState>,
    crate::auth::AuthUser(wallet): crate::auth::AuthUser,
    headers: axum::http::HeaderMap,
) -> Result<(StatusCode, Json<EnqueueJobResponse>), AppError> {
    crate::admin::require_admin_access(
        &state,
        &wallet,
        &headers,
        crate::admin::AdminRole::RiskAdmin,
        true,
    )
    .await?;
    let id = enqueue(
        &state.db,
        "reconcile_accounts",
        &serde_json::Value::Null,
        None,
        3,
        60,
    )
    .await
    .map_err(|e| db_error("enqueue account reconciliation", e))?;
    Ok((StatusCode::ACCEPTED, Json(EnqueueJobResponse { id })))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_state() -> (AppState, std::path::PathBuf) {
        let db_path =
            std::env::temp_dir().join(format!("zenith-jobs-test-{}.db", uuid::Uuid::new_v4()));
        let pool = crate::db::init_pool(&format!("sqlite://{}", db_path.display())).await;
        (AppState::new(pool), db_path)
    }

    #[tokio::test]
    async fn unique_jobs_are_deduplicated_while_active_and_can_repeat_after_success() {
        let (state, path) = test_state().await;
        let key = format!("once:{}", uuid::Uuid::new_v4());
        let first = enqueue(
            &state.db,
            "auth_cleanup",
            &serde_json::Value::Null,
            Some(&key),
            2,
            10,
        )
        .await
        .unwrap();
        let duplicate = enqueue(
            &state.db,
            "auth_cleanup",
            &serde_json::Value::Null,
            Some(&key),
            2,
            10,
        )
        .await
        .unwrap();
        assert_eq!(first, duplicate);

        assert!(run_one(&state, "test-worker").await.unwrap());
        let repeated = enqueue(
            &state.db,
            "auth_cleanup",
            &serde_json::Value::Null,
            Some(&key),
            2,
            10,
        )
        .await
        .unwrap();
        assert_ne!(first, repeated);
        let metric = metrics(&state.db).await.unwrap();
        assert_eq!(metric.succeeded, 1);
        assert_eq!(metric.attempts, 1);

        state.db.close().await;
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn failing_job_retries_then_moves_to_dead_letter() {
        let (state, path) = test_state().await;
        let id = enqueue(
            &state.db,
            "unsupported",
            &serde_json::Value::Null,
            None,
            1,
            10,
        )
        .await
        .unwrap();
        assert!(run_one(&state, "test-worker").await.unwrap());
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(status, "dead");
        let metric = metrics(&state.db).await.unwrap();
        assert_eq!(metric.dead, 1);
        assert_eq!(metric.failed_attempts, 1);

        state.db.close().await;
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn job_timeout_is_recorded_as_failed_attempt() {
        let (state, path) = test_state().await;
        enqueue(
            &state.db,
            "test_sleep",
            &serde_json::Value::Null,
            None,
            1,
            1,
        )
        .await
        .unwrap();
        run_one(&state, "test-worker").await.unwrap();
        let outcome: String = sqlx::query_scalar("SELECT outcome FROM job_attempts")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(outcome, "timeout");
        let metric = metrics(&state.db).await.unwrap();
        assert_eq!(metric.failed_attempts, 1);

        state.db.close().await;
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn export_job_persists_a_retrievable_artifact() {
        let (state, path) = test_state().await;
        let id = enqueue(&state.db, "export", &serde_json::Value::Null, None, 1, 10)
            .await
            .unwrap();
        run_one(&state, "test-worker").await.unwrap();
        let artifact: String =
            sqlx::query_scalar("SELECT content_json FROM job_artifacts WHERE job_id = ?")
                .bind(id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&artifact).unwrap();
        assert!(value["accounts"].is_array());
        assert!(value["positions"].is_array());

        state.db.close().await;
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn reconciliation_records_collateral_mismatches() {
        let (state, path) = test_state().await;
        sqlx::query(
            "INSERT INTO accounts (wallet_address, collateral_locked) VALUES ('GTEST', 12.0)",
        )
        .execute(&state.db)
        .await
        .unwrap();
        let job_id = enqueue(
            &state.db,
            "reconcile_accounts",
            &serde_json::Value::Null,
            None,
            1,
            10,
        )
        .await
        .unwrap();
        run_one(&state, "test-worker").await.unwrap();
        let differences: i64 =
            sqlx::query_scalar("SELECT differences FROM reconciliation_runs WHERE job_id = ?")
                .bind(job_id)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(differences, 1);

        state.db.close().await;
        let _ = std::fs::remove_file(path);
    }
}
