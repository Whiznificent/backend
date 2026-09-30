use sqlx::SqlitePool;

pub struct WatchlistRepo;

impl WatchlistRepo {
    pub async fn list_by_wallet(pool: &SqlitePool, wallet_address: &str) -> Result<Vec<String>, sqlx::Error> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT underlying FROM watchlist WHERE wallet_address = ? ORDER BY added_at ASC")
            .bind(wallet_address)
            .fetch_all(pool)
            .await?;
        Ok(rows.into_iter().map(|(u,)| u).collect())
    }

    pub async fn add(pool: &SqlitePool, wallet_address: &str, underlying: &str) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO watchlist (wallet_address, underlying) VALUES (?, ?) ON CONFLICT(wallet_address, underlying) DO NOTHING")
            .bind(wallet_address)
            .bind(underlying)
            .execute(pool)
            .await?;
        Ok(())
    }

    pub async fn remove(pool: &SqlitePool, wallet_address: &str, underlying: &str) -> Result<bool, sqlx::Error> {
        let rows = sqlx::query("DELETE FROM watchlist WHERE wallet_address = ? AND underlying = ?")
            .bind(wallet_address)
            .bind(underlying)
            .execute(pool)
            .await?
            .rows_affected();
        Ok(rows > 0)
    }
}
