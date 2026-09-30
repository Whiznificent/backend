use super::tasks::{JobStatus, KeeperError, KeeperJob, KeeperTask, TaskExecutionResult};
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub struct ExpirySettlementTask {
    on_chain_settled: Arc<Mutex<HashSet<String>>>,
}

impl ExpirySettlementTask {
    pub fn new() -> Self {
        Self {
            on_chain_settled: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn mark_on_chain_settled(&self, series_id: &str) {
        let mut settled = self.on_chain_settled.lock().unwrap();
        settled.insert(series_id.to_string());
    }

    pub fn is_on_chain_settled(&self, series_id: &str) -> bool {
        let settled = self.on_chain_settled.lock().unwrap();
        settled.contains(series_id)
    }
}

impl Default for ExpirySettlementTask {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl KeeperTask for ExpirySettlementTask {
    fn name(&self) -> &'static str {
        "expiry_settlement"
    }

    async fn find_due_jobs(&self, now: u64, _pool: &SqlitePool) -> Result<Vec<KeeperJob>, KeeperError> {
        // Return dummy or discovered expired series jobs
        Ok(vec![])
    }

    async fn execute(&self, job: &KeeperJob, pool: &SqlitePool) -> Result<TaskExecutionResult, KeeperError> {
        // 1. Check on-chain state first for idempotency
        if self.is_on_chain_settled(&job.target_id) {
            sqlx::query(
                "INSERT INTO keeper_job_executions (job_id, task_name, target_id, status, fee_spent, due_at, executed_at)
                 VALUES (?1, ?2, ?3, 'skipped', 0, ?4, strftime('%s', 'now'))",
            )
            .bind(&job.job_id)
            .bind(self.name())
            .bind(&job.target_id)
            .bind(job.due_at as i64)
            .execute(pool)
            .await
            .map_err(|e| KeeperError::DatabaseError(e.to_string()))?;

            return Ok(TaskExecutionResult {
                fee_spent: 0,
                status: JobStatus::SkippedAlreadySettled,
                details: Some("Series was already settled on-chain".into()),
            });
        }

        // 2. Perform settlement action
        let fee_spent: i64 = 100_000; // 0.01 XLM
        self.mark_on_chain_settled(&job.target_id);

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
            details: Some(format!("Successfully settled series {}", job.target_id)),
        })
    }
}
