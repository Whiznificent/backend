use crate::models::Position;
use sqlx::SqlitePool;

pub struct StrategyRepo;

impl StrategyRepo {
    pub async fn list_by_wallet(pool: &SqlitePool, wallet_address: &str) -> Result<Vec<Position>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM positions WHERE wallet_address = ? AND strategy_id IS NOT NULL ORDER BY opened_at DESC")
            .bind(wallet_address)
            .fetch_all(pool)
            .await
    }

    pub async fn get_legs_by_strategy_id(
        pool: &SqlitePool,
        strategy_id: &str,
        wallet_address: &str,
    ) -> Result<Vec<Position>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM positions WHERE strategy_id = ? AND wallet_address = ?")
            .bind(strategy_id)
            .bind(wallet_address)
            .fetch_all(pool)
            .await
    }
}
