use sqlx::SqlitePool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Account {
    pub wallet_address: String,
    pub balance: f64,
    pub collateral_locked: f64,
    pub created_at: String,
}

pub struct AccountRepo;

impl AccountRepo {
    pub async fn get_or_create(pool: &SqlitePool, wallet_address: &str) -> Result<Account, sqlx::Error> {
        sqlx::query(
            "INSERT INTO accounts (wallet_address) VALUES (?) ON CONFLICT(wallet_address) DO NOTHING",
        )
        .bind(wallet_address)
        .execute(pool)
        .await?;

        sqlx::query_as("SELECT * FROM accounts WHERE wallet_address = ?")
            .bind(wallet_address)
            .fetch_one(pool)
            .await
    }

    pub async fn get(pool: &SqlitePool, wallet_address: &str) -> Result<Option<Account>, sqlx::Error> {
        sqlx::query_as("SELECT * FROM accounts WHERE wallet_address = ?")
            .bind(wallet_address)
            .fetch_optional(pool)
            .await
    }

    pub async fn update_balances(
        pool: &SqlitePool,
        wallet_address: &str,
        balance_delta: f64,
        collateral_delta: f64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE accounts SET 
                balance = balance + ?1,
                collateral_locked = collateral_locked + ?2
             WHERE wallet_address = ?3",
        )
        .bind(balance_delta)
        .bind(collateral_delta)
        .bind(wallet_address)
        .execute(pool)
        .await?;
        Ok(())
    }
}
