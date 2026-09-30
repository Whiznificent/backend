use zenith_backend::retention::{RetentionPolicy, RetentionService};

#[tokio::test]
async fn test_retention_denylist_enforcement() {
    let db_path = std::env::temp_dir().join(format!("zenith-retention-test-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let protected_policy = RetentionPolicy {
        table_name: "sponsored_transactions".into(),
        age_column: "created_at".into(),
        retention_days: 30,
        archive: true,
        batch_size: 1000,
    };

    let res = RetentionService::execute_purge_policy(&pool, &protected_policy).await;
    assert!(res.is_err());
    assert!(res.unwrap_err().contains("protected denylist"));

    let _ = std::fs::remove_file(db_path);
}
