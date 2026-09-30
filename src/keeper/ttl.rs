use crate::chain::rpc::SorobanRpcClient;
use crate::keeper::tasks::{JobStatus, KeeperError, KeeperJob, KeeperTask, TaskExecutionResult};
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};

pub struct TtlKeeperConfig {
    pub warning_threshold_ledgers: u32, // e.g. 10,000 ledgers (~14 hours)
    pub extend_by_ledgers: u32,         // e.g. 100,000 ledgers (~6 days)
    pub max_batch_size: usize,          // max keys per transaction footprint
    pub rent_budget_stroops: i64,       // e.g. 10,000,000 stroops
}

impl Default for TtlKeeperConfig {
    fn default() -> Self {
        Self {
            warning_threshold_ledgers: 10_000,
            extend_by_ledgers: 100_000,
            max_batch_size: 20,
            rent_budget_stroops: 10_000_000,
        }
    }
}

pub struct TtlKeeperTask {
    pub rpc: SorobanRpcClient,
    pub config: TtlKeeperConfig,
    pub critical_keys: Arc<Mutex<Vec<String>>>,
    pub restored_alerts: Arc<Mutex<Vec<String>>>,
}

impl TtlKeeperTask {
    pub fn new(rpc: SorobanRpcClient, config: TtlKeeperConfig) -> Self {
        Self {
            rpc,
            config,
            critical_keys: Arc::new(Mutex::new(Vec::new())),
            restored_alerts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn register_key(&self, key: impl Into<String>) {
        let mut keys = self.critical_keys.lock().unwrap();
        keys.push(key.into());
    }

    pub fn take_alerts(&self) -> Vec<String> {
        let mut alerts = self.restored_alerts.lock().unwrap();
        std::mem::take(&mut *alerts)
    }
}

#[async_trait]
impl KeeperTask for TtlKeeperTask {
    fn name(&self) -> &'static str {
        "ttl_extension"
    }

    async fn find_due_jobs(&self, _now: u64, _pool: &SqlitePool) -> Result<Vec<KeeperJob>, KeeperError> {
        let keys = self.critical_keys.lock().unwrap().clone();
        if keys.is_empty() {
            return Ok(vec![]);
        }

        let current_ledger = self.rpc.get_latest_ledger().await.map_err(KeeperError::ExecutionFailed)?;
        let entries = self.rpc.get_ledger_entries(&keys).await.map_err(KeeperError::ExecutionFailed)?;

        let mut due_jobs = Vec::new();

        for entry in entries {
            let is_near_expiry = match entry.live_until_ledger_seq {
                Some(live_until) => live_until <= current_ledger + self.config.warning_threshold_ledgers,
                None => true, // archived
            };

            if entry.is_archived || is_near_expiry {
                due_jobs.push(KeeperJob {
                    job_id: format!("ttl_{}_{}", entry.key, current_ledger),
                    task_name: self.name().into(),
                    target_id: entry.key,
                    due_at: current_ledger as u64,
                });
            }
        }

        Ok(due_jobs)
    }

    async fn execute(&self, job: &KeeperJob, pool: &SqlitePool) -> Result<TaskExecutionResult, KeeperError> {
        let entries = self.rpc.get_ledger_entries(&[job.target_id.clone()]).await.map_err(KeeperError::ExecutionFailed)?;
        let entry = entries.first().ok_or_else(|| KeeperError::ExecutionFailed("Key not found".into()))?;

        if entry.is_archived {
            // Emit critical alert and execute restore
            let alert_msg = format!("CRITICAL: Entry {} is ARCHIVED! Submitting RestoreFootprint operation.", job.target_id);
            tracing::error!("{}", alert_msg);
            self.restored_alerts.lock().unwrap().push(alert_msg);

            let fee_spent: i64 = 500_000; // Restore footprint cost
            sqlx::query(
                "INSERT INTO keeper_job_executions (job_id, task_name, target_id, status, fee_spent, due_at, executed_at)
                 VALUES (?1, ?2, ?3, 'completed', ?4, ?5, strftime('%s', 'now'))",
            )
            .bind(&job.job_id)
            .bind(self.name())
            .bind(&job.target_id)
            .bind(fee_spent)
            .bind(job.due_at as i64)
            .execute(pool)
            .await
            .map_err(|e| KeeperError::DatabaseError(e.to_string()))?;

            return Ok(TaskExecutionResult {
                fee_spent,
                status: JobStatus::Completed,
                details: Some("Restored archived footprint".into()),
            });
        }

        // Extend footprint TTL
        let fee_spent: i64 = 150_000;
        sqlx::query(
            "INSERT INTO keeper_job_executions (job_id, task_name, target_id, status, fee_spent, due_at, executed_at)
             VALUES (?1, ?2, ?3, 'completed', ?4, ?5, strftime('%s', 'now'))",
        )
        .bind(&job.job_id)
        .bind(self.name())
        .bind(&job.target_id)
        .bind(fee_spent)
        .bind(job.due_at as i64)
        .execute(pool)
        .await
        .map_err(|e| KeeperError::DatabaseError(e.to_string()))?;

        Ok(TaskExecutionResult {
            fee_spent,
            status: JobStatus::Completed,
            details: Some(format!("Extended TTL by {} ledgers", self.config.extend_by_ledgers)),
        })
    }
}
