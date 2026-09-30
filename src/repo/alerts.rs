use sqlx::SqlitePool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Alert {
    pub id: String,
    pub wallet_address: String,
    pub underlying: String,
    pub target_price: f64,
    pub direction: String,
    pub status: String,
    pub created_at: String,
    pub triggered_at: Option<String>,
}

pub struct AlertRepo;

impl AlertRepo {
    pub async fn list_by_wallet(pool: &SqlitePool, wallet_address: &str) -> Result<Vec<Alert>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM alerts WHERE wallet_address = ? ORDER BY created_at DESC")
            .bind(wallet_address)
            .fetch_all(pool)
            .await
    }

    pub async fn list_active(pool: &SqlitePool) -> Result<Vec<Alert>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM alerts WHERE status = 'active'")
            .fetch_all(pool)
            .await
    }

    pub async fn insert(pool: &SqlitePool, alert: &Alert) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO alerts (id, wallet_address, underlying, target_price, direction, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&alert.id)
        .bind(&alert.wallet_address)
        .bind(&alert.underlying)
        .bind(alert.target_price)
        .bind(&alert.direction)
        .bind(&alert.status)
        .bind(&alert.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn delete_by_id_and_wallet(pool: &SqlitePool, id: &str, wallet_address: &str) -> Result<bool, sqlx::Error> {
        let rows = sqlx::query("DELETE FROM alerts WHERE id = ? AND wallet_address = ?")
            .bind(id)
            .bind(wallet_address)
            .execute(pool)
            .await?
            .rows_affected();
        Ok(rows > 0)
    }
}
