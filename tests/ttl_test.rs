use zenith_backend::chain::rpc::SorobanRpcClient;
use zenith_backend::keeper::tasks::{JobStatus, KeeperTask};
use zenith_backend::keeper::ttl::{TtlKeeperConfig, TtlKeeperTask};

#[tokio::test]
async fn test_ttl_extension_for_expiring_entry() {
    let db_path = std::env::temp_dir().join(format!("zenith-ttl-test-1-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let rpc = SorobanRpcClient::new("https://soroban-testnet.stellar.org".into());
    // Entry expiring at ledger 1_005_000 (within 10_000 threshold of current ledger 1_000_000)
    let key = "CONTRACT_INSTANCE_KEY";
    rpc.insert_mock_entry(key, Some(1_005_000), false);

    let task = TtlKeeperTask::new(rpc, TtlKeeperConfig::default());
    task.register_key(key);

    let jobs = task.find_due_jobs(1_000_000, &pool).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].target_id, key);

    let result = task.execute(&jobs[0], &pool).await.unwrap();
    assert_eq!(result.status, JobStatus::Completed);
    assert_eq!(result.fee_spent, 150_000);

    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn test_ttl_restores_archived_entry_and_alerts() {
    let db_path = std::env::temp_dir().join(format!("zenith-ttl-test-2-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let rpc = SorobanRpcClient::new("https://soroban-testnet.stellar.org".into());
    let key = "SERIES_ARCHIVED_KEY";
    rpc.insert_mock_entry(key, None, true);

    let task = TtlKeeperTask::new(rpc, TtlKeeperConfig::default());
    task.register_key(key);

    let jobs = task.find_due_jobs(1_000_000, &pool).await.unwrap();
    assert_eq!(jobs.len(), 1);

    let result = task.execute(&jobs[0], &pool).await.unwrap();
    assert_eq!(result.status, JobStatus::Completed);
    assert_eq!(result.fee_spent, 500_000);

    let alerts = task.take_alerts();
    assert_eq!(alerts.len(), 1);
    assert!(alerts[0].contains("CRITICAL"));
    assert!(alerts[0].contains("RestoreFootprint"));

    let _ = std::fs::remove_file(db_path);
}
