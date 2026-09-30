#[tokio::test]
async fn test_sqlite_foreign_key_enforcement() {
    let db_path = std::env::temp_dir().join(format!("zenith-fk-test-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    // Attempting to insert a position with a non-existent wallet account must fail due to FK enforcement
    let res = sqlx::query(
        "INSERT INTO positions (id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status)
         VALUES ('p1', 'NON_EXISTENT_WALLET', 'XLM', 0.12, 14, 'call', 'long', 10, 0.01, 0.12, 0, 'open')",
    )
    .execute(&pool)
    .await;

    assert!(res.is_err(), "Foreign key enforcement should reject non-existent account");

    let _ = std::fs::remove_file(db_path);
}
