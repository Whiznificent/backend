use crate::models::Position;
use sqlx::SqlitePool;

pub struct PositionRepo;

impl PositionRepo {
    pub async fn list_open_by_wallet(pool: &SqlitePool, wallet_address: &str) -> Result<Vec<Position>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM positions WHERE wallet_address = ? AND status = 'open' ORDER BY opened_at DESC")
            .bind(wallet_address)
            .fetch_all(pool)
            .await
    }

    pub async fn list_history_by_wallet(pool: &SqlitePool, wallet_address: &str) -> Result<Vec<Position>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM positions WHERE wallet_address = ? ORDER BY opened_at DESC")
            .bind(wallet_address)
            .fetch_all(pool)
            .await
    }

    pub async fn get_by_id_and_wallet(
        pool: &SqlitePool,
        id: &str,
        wallet_address: &str,
    ) -> Result<Option<Position>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM positions WHERE id = ? AND wallet_address = ?")
            .bind(id)
            .bind(wallet_address)
            .fetch_optional(pool)
            .await
    }

    pub async fn insert(
        pool: &SqlitePool,
        pos: &Position,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO positions (id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, opened_at, strategy_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
        )
        .bind(&pos.id)
        .bind(&pos.wallet_address)
        .bind(&pos.underlying)
        .bind(pos.strike)
        .bind(pos.expiry_days)
        .bind(&pos.option_type)
        .bind(&pos.position_type)
        .bind(pos.contracts)
        .bind(pos.entry_premium)
        .bind(pos.entry_spot)
        .bind(pos.collateral)
        .bind(&pos.status)
        .bind(&pos.opened_at)
        .bind(&pos.strategy_id)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn close(
        pool: &SqlitePool,
        id: &str,
        wallet_address: &str,
        close_premium: f64,
        close_spot: f64,
        realized_pnl: f64,
        closed_at: &str,
    ) -> Result<bool, sqlx::Error> {
        let rows_affected = sqlx::query(
            "UPDATE positions SET 
                status = 'closed',
                close_premium = ?1,
                close_spot = ?2,
                realized_pnl = ?3,
                closed_at = ?4
             WHERE id = ?5 AND wallet_address = ?6 AND status = 'open'",
        )
        .bind(close_premium)
        .bind(close_spot)
        .bind(realized_pnl)
        .bind(closed_at)
        .bind(id)
        .bind(wallet_address)
        .execute(pool)
        .await?
        .rows_affected();

        Ok(rows_affected > 0)
    }
}
