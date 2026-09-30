use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeeperJob {
    pub job_id: String,
    pub task_name: String,
    pub target_id: String,
    pub due_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Completed,
    SkippedAlreadySettled,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct TaskExecutionResult {
    pub fee_spent: i64,
    pub status: JobStatus,
    pub details: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeeperError {
    LeadershipLost,
    ExecutionFailed(String),
    DatabaseError(String),
}

impl std::fmt::Display for KeeperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LeadershipLost => write!(f, "Keeper lost leadership lease during execution"),
            Self::ExecutionFailed(e) => write!(f, "Keeper job execution failed: {e}"),
            Self::DatabaseError(e) => write!(f, "Keeper database error: {e}"),
        }
    }
}

impl std::error::Error for KeeperError {}

#[async_trait]
pub trait KeeperTask: Send + Sync {
    fn name(&self) -> &'static str;
    async fn find_due_jobs(&self, now: u64, pool: &SqlitePool) -> Result<Vec<KeeperJob>, KeeperError>;
    async fn execute(&self, job: &KeeperJob, pool: &SqlitePool) -> Result<TaskExecutionResult, KeeperError>;
}
