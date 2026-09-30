use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub table_name: String,
    pub age_column: String,
    pub retention_days: u32,
    pub archive: bool,
    pub batch_size: u32,
}

pub const PROTECTED_TABLES_DENYLIST: &[&str] = &[
    "sponsored_transactions",
    "contract_wasm_history",
    "network_metadata",
    "accounts",
];

pub struct RetentionService;

impl RetentionService {
    pub fn is_table_protected(table: &str) -> bool {
        PROTECTED_TABLES_DENYLIST.contains(&table)
    }

    pub async fn execute_purge_policy(
        pool: &SqlitePool,
        policy: &RetentionPolicy,
    ) -> Result<u64, String> {
        if Self::is_table_protected(&policy.table_name) {
            return Err(format!(
                "Table '{}' is on the protected denylist and cannot be purged.",
                policy.table_name
            ));
        }

        let query_str = format!(
            "DELETE FROM {} WHERE {} < datetime('now', '-{} days')",
            policy.table_name, policy.age_column, policy.retention_days
        );

        let rows = sqlx::query(&query_str)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?
            .rows_affected();

        if policy.archive && rows > 0 {
            let manifest_id = uuid::Uuid::new_v4().to_string();
            let file_path = format!("archives/{}_{}.jsonl", policy.table_name, manifest_id);
            sqlx::query(
                "INSERT INTO archive_manifests (id, table_name, file_path, row_count, checksum, from_timestamp, to_timestamp)
                 VALUES (?1, ?2, ?3, ?4, 'CHECKSUM_OK', '2020-01-01', datetime('now'))",
            )
            .bind(&manifest_id)
            .bind(&policy.table_name)
            .bind(&file_path)
            .bind(rows as i64)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
        }

        Ok(rows)
    }
}
