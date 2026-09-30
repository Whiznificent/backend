//! Verifies the database-level invariants added by migration 0007: the CHECK
//! constraints on accounts/positions and the integrity triggers that reject
//! illegal status transitions and edits to immutable columns.
//!
//! Each test spins up its own throwaway database with the full migration
//! chain applied, so the new schema is in place.

use zenith_backend::db::init_pool;

async fn test_db() -> (sqlx::SqlitePool, std::path::PathBuf) {
    let db_path = std::env::temp_dir().join(format!(
        "zenith-invariants-test-{}.db",
        uuid::Uuid::new_v4()
    ));
    let pool = init_pool(&format!("sqlite://{}", db_path.display())).await;
    (pool, db_path)
}

/// Inserts a valid open long call for `wallet` and returns its id.
async fn insert_open_position(pool: &sqlx::SqlitePool, wallet: &str, id: &str) {
    sqlx::query(
        "INSERT INTO positions
            (id, wallet_address, underlying, strike, expiry_days, option_type,
             position_type, contracts, entry_premium, entry_spot, status)
         VALUES (?, ?, 'BTC', 70000, 30, 'call', 'long', 1, 100, 67000, 'open')",
    )
    .bind(id)
    .bind(wallet)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn accounts_reject_negative_balance_and_collateral() {
    let (pool, db_path) = test_db().await;
    sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('GTEST')")
        .execute(&pool)
        .await
        .unwrap();

    let negative_balance = sqlx::query("UPDATE accounts SET balance = -1 WHERE wallet_address = 'GTEST'")
        .execute(&pool)
        .await;
    assert!(negative_balance.is_err(), "balance < 0 must be rejected");

    let negative_collateral =
        sqlx::query("UPDATE accounts SET collateral_locked = -1 WHERE wallet_address = 'GTEST'")
            .execute(&pool)
            .await;
    assert!(negative_collateral.is_err(), "collateral_locked < 0 must be rejected");

    pool.close().await;
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn positions_reject_non_positive_contracts_and_strike() {
    let (pool, db_path) = test_db().await;
    sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('GTEST')")
        .execute(&pool)
        .await
        .unwrap();

    let bad_contracts = sqlx::query(
        "INSERT INTO positions
            (id, wallet_address, underlying, strike, expiry_days, option_type,
             position_type, contracts, entry_premium, entry_spot, status)
         VALUES ('p1', 'GTEST', 'BTC', 70000, 30, 'call', 'long', 0, 100, 67000, 'open')",
    )
    .execute(&pool)
    .await;
    assert!(bad_contracts.is_err(), "contracts <= 0 must be rejected");

    let bad_strike = sqlx::query(
        "INSERT INTO positions
            (id, wallet_address, underlying, strike, expiry_days, option_type,
             position_type, contracts, entry_premium, entry_spot, status)
         VALUES ('p1', 'GTEST', 'BTC', 0, 30, 'call', 'long', 1, 100, 67000, 'open')",
    )
    .execute(&pool)
    .await;
    assert!(bad_strike.is_err(), "strike <= 0 must be rejected");

    pool.close().await;
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn positions_enforce_status_column_coherence() {
    let (pool, db_path) = test_db().await;
    sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('GTEST')")
        .execute(&pool)
        .await
        .unwrap();

    // An open position carrying a settlement column is incoherent.
    let open_with_close = sqlx::query(
        "INSERT INTO positions
            (id, wallet_address, underlying, strike, expiry_days, option_type,
             position_type, contracts, entry_premium, entry_spot, status, close_premium)
         VALUES ('p1', 'GTEST', 'BTC', 70000, 30, 'call', 'long', 1, 100, 67000, 'open', 5)",
    )
    .execute(&pool)
    .await;
    assert!(open_with_close.is_err(), "open position with close_premium must be rejected");

    // A closed position missing its settlement columns is incoherent.
    let closed_missing = sqlx::query(
        "INSERT INTO positions
            (id, wallet_address, underlying, strike, expiry_days, option_type,
             position_type, contracts, entry_premium, entry_spot, status)
         VALUES ('p1', 'GTEST', 'BTC', 70000, 30, 'call', 'long', 1, 100, 67000, 'closed')",
    )
    .execute(&pool)
    .await;
    assert!(closed_missing.is_err(), "closed position without settlement must be rejected");

    pool.close().await;
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn trigger_rejects_illegal_status_transition() {
    let (pool, db_path) = test_db().await;
    sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('GTEST')")
        .execute(&pool)
        .await
        .unwrap();
    insert_open_position(&pool, "GTEST", "p1").await;

    // open -> closed is legal.
    sqlx::query("UPDATE positions SET status = 'closed' WHERE id = 'p1'")
        .execute(&pool)
        .await
        .unwrap();

    // closed -> open is not.
    let illegal = sqlx::query("UPDATE positions SET status = 'open' WHERE id = 'p1'")
        .execute(&pool)
        .await;
    assert!(illegal.is_err(), "closed -> open must be rejected");

    // closed -> rolled is legal (a roll relabels the just-closed leg).
    sqlx::query("UPDATE positions SET status = 'rolled' WHERE id = 'p1'")
        .execute(&pool)
        .await
        .unwrap();

    // rolled -> closed is not.
    let illegal = sqlx::query("UPDATE positions SET status = 'closed' WHERE id = 'p1'")
        .execute(&pool)
        .await;
    assert!(illegal.is_err(), "rolled -> closed must be rejected");

    pool.close().await;
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn trigger_forbids_edits_to_immutable_columns() {
    let (pool, db_path) = test_db().await;
    sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('GTEST')")
        .execute(&pool)
        .await
        .unwrap();
    insert_open_position(&pool, "GTEST", "p1").await;

    let edit_entry_premium =
        sqlx::query("UPDATE positions SET entry_premium = 999 WHERE id = 'p1'")
            .execute(&pool)
            .await;
    assert!(edit_entry_premium.is_err(), "entry_premium is immutable");

    let edit_opened_at = sqlx::query("UPDATE positions SET opened_at = '2000-01-01T00:00:00.000Z' WHERE id = 'p1'")
        .execute(&pool)
        .await;
    assert!(edit_opened_at.is_err(), "opened_at is immutable");

    let edit_wallet = sqlx::query("UPDATE positions SET wallet_address = 'OTHER' WHERE id = 'p1'")
        .execute(&pool)
        .await;
    assert!(edit_wallet.is_err(), "wallet_address is immutable");

    // A settlement write (the only kind the app does) still works: it touches
    // status and the settlement columns, none of which are immutable.
    sqlx::query("UPDATE positions SET status = 'closed', close_premium = 5, close_spot = 67000, realized_pnl = -95, closed_at = '2024-01-01T00:00:00.000Z' WHERE id = 'p1'")
        .execute(&pool)
        .await
        .unwrap();

    pool.close().await;
    let _ = std::fs::remove_file(&db_path);
}
