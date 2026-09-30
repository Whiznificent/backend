use sqlx::SqlitePool;

pub struct SessionRepo;

impl SessionRepo {
    pub async fn get_wallet_by_token(pool: &SqlitePool, token: &str, now: &str) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT wallet_address FROM sessions WHERE token = ? AND expires_at > ?",
        )
        .bind(token)
        .bind(now)
        .fetch_optional(pool)
        .await?;

        Ok(row.map(|(w,)| w))
    }

    pub async fn create_session(
        pool: &SqlitePool,
        token: &str,
        wallet_address: &str,
        expires_at: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO sessions (token, wallet_address, expires_at) VALUES (?, ?, ?)")
            .bind(token)
            .bind(wallet_address)
            .bind(expires_at)
            .execute(pool)
            .await?;
        Ok(())
    }

    pub async fn cleanup_expired(pool: &SqlitePool, now: &str) -> Result<u64, sqlx::Error> {
        let rows = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now)
            .execute(pool)
            .await?
            .rows_affected();
        Ok(rows)
    }
}
