use zenith_backend::keeper::lease::LeaseManager;
use zenith_backend::keeper::settlement::ExpirySettlementTask;
use zenith_backend::keeper::tasks::{JobStatus, KeeperJob, KeeperTask};

#[tokio::test]
async fn test_two_instance_lease_contention_and_failover() {
    let db_path = std::env::temp_dir().join(format!("zenith-lease-test-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let lease_name = "expiry_settlement_leader";
    let instance_a = LeaseManager::new(lease_name, "instance-A", 2); // 2s TTL
    let instance_b = LeaseManager::new(lease_name, "instance-B", 2);

    // Instance A acquires lease first
    let acquired_a = instance_a.try_acquire(&pool).await.unwrap();
    assert!(acquired_a);
    assert!(instance_a.is_leader(&pool).await.unwrap());

    // Instance B tries to acquire while A is active -> should fail
    let acquired_b = instance_b.try_acquire(&pool).await.unwrap();
    assert!(!acquired_b);
    assert!(!instance_b.is_leader(&pool).await.unwrap());

    // Instance A releases lease
    instance_a.release(&pool).await.unwrap();

    // Now Instance B can acquire
    let acquired_b_after = instance_b.try_acquire(&pool).await.unwrap();
    assert!(acquired_b_after);
    assert!(instance_b.is_leader(&pool).await.unwrap());

    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn test_idempotent_no_double_settle() {
    let db_path = std::env::temp_dir().join(format!("zenith-settle-test-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let task = ExpirySettlementTask::new();
    let job = KeeperJob {
        job_id: "JOB_123".into(),
        task_name: "expiry_settlement".into(),
        target_id: "SERIES_XLM_CALL_012".into(),
        due_at: 1700000000,
    };

    // First execution succeeds
    let res1 = task.execute(&job, &pool).await.unwrap();
    assert_eq!(res1.status, JobStatus::Completed);
    assert_eq!(res1.fee_spent, 100_000);

    // Second execution with same series is skipped without performing action again
    let job2 = KeeperJob {
        job_id: "JOB_124".into(),
        task_name: "expiry_settlement".into(),
        target_id: "SERIES_XLM_CALL_012".into(),
        due_at: 1700000000,
    };
    let res2 = task.execute(&job2, &pool).await.unwrap();
    assert_eq!(res2.status, JobStatus::SkippedAlreadySettled);
    assert_eq!(res2.fee_spent, 0);

    let _ = std::fs::remove_file(db_path);
}
