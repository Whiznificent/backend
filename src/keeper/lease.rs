use sqlx::SqlitePool;

#[derive(Debug, PartialEq, Eq)]
pub enum LeaseError {
    DatabaseError(String),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DatabaseError(e) => write!(f, "Lease database error: {e}"),
        }
    }
}

impl std::error::Error for LeaseError {}

#[derive(Clone, Debug)]
pub struct LeaseManager {
    pub name: String,
    pub holder_id: String,
    pub ttl_seconds: i64,
}

impl LeaseManager {
    pub fn new(name: impl Into<String>, holder_id: impl Into<String>, ttl_seconds: i64) -> Self {
        Self {
            name: name.into(),
            holder_id: holder_id.into(),
            ttl_seconds,
        }
    }

    /// Atomically acquires or takes over an expired lease using DB-evaluated timestamps
    pub async fn try_acquire(&self, pool: &SqlitePool) -> Result<bool, LeaseError> {
        // Attempt atomic upsert where expired or unheld
        let rows_affected = sqlx::query(
            "INSERT INTO leases (name, holder, expires_at, acquired_at, renewed_at)
             VALUES (?1, ?2, strftime('%s', 'now') + ?3, strftime('%s', 'now'), strftime('%s', 'now'))
             ON CONFLICT(name) DO UPDATE SET
                holder = ?2,
                expires_at = strftime('%s', 'now') + ?3,
                acquired_at = strftime('%s', 'now'),
                renewed_at = strftime('%s', 'now')
             WHERE leases.expires_at <= strftime('%s', 'now') OR leases.holder = ?2",
        )
        .bind(&self.name)
        .bind(&self.holder_id)
        .bind(self.ttl_seconds)
        .execute(pool)
        .await
        .map_err(|e| LeaseError::DatabaseError(e.to_string()))?
        .rows_affected();

        Ok(rows_affected > 0)
    }

    /// Renews an active lease held by this instance
    pub async fn renew(&self, pool: &SqlitePool) -> Result<bool, LeaseError> {
        let rows_affected = sqlx::query(
            "UPDATE leases SET
                expires_at = strftime('%s', 'now') + ?1,
                renewed_at = strftime('%s', 'now')
             WHERE name = ?2 AND holder = ?3 AND expires_at > strftime('%s', 'now')",
        )
        .bind(self.ttl_seconds)
        .bind(&self.name)
        .bind(&self.holder_id)
        .execute(pool)
        .await
        .map_err(|e| LeaseError::DatabaseError(e.to_string()))?
        .rows_affected();

        Ok(rows_affected > 0)
    }

    /// Verifies if this instance is still the active, unexpired lease holder
    pub async fn is_leader(&self, pool: &SqlitePool) -> Result<bool, LeaseError> {
        let is_holder: Option<bool> = sqlx::query_scalar(
            "SELECT (holder = ?1 AND expires_at > strftime('%s', 'now'))
             FROM leases WHERE name = ?2",
        )
        .bind(&self.holder_id)
        .bind(&self.name)
        .fetch_optional(pool)
        .await
        .map_err(|e| LeaseError::DatabaseError(e.to_string()))?;

        Ok(is_holder.unwrap_or(false))
    }

    /// Explicitly releases the lease on clean shutdown
    pub async fn release(&self, pool: &SqlitePool) -> Result<(), LeaseError> {
        sqlx::query(
            "UPDATE leases SET expires_at = strftime('%s', 'now')
             WHERE name = ?1 AND holder = ?2",
        )
        .bind(&self.name)
        .bind(&self.holder_id)
        .execute(pool)
        .await
        .map_err(|e| LeaseError::DatabaseError(e.to_string()))?;

        Ok(())
    }
}
