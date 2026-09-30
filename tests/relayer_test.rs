use std::sync::Arc;
use zenith_backend::chain::relayer::{FeeBumpRelayer, RelayerError, SponsorshipPolicy};
use zenith_backend::signing::SponsorKeyPool;

#[tokio::test]
async fn test_relayer_foreign_transaction_rejected() {
    let db_path = std::env::temp_dir().join(format!("zenith-relayer-1-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let key_pool = Arc::new(SponsorKeyPool::random_single());
    let relayer = FeeBumpRelayer::new(SponsorshipPolicy::default(), key_pool, pool);

    let res = relayer
        .sponsor_and_wrap("UNAUTHORIZED_HASH_123", "ENVELOPE_XDR", "G_WALLET", 1000, 100.0)
        .await;

    assert_eq!(res, Err(RelayerError::UnauthorizedTransaction));
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn test_relayer_critical_watermark_pause() {
    let db_path = std::env::temp_dir().join(format!("zenith-relayer-2-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let key_pool = Arc::new(SponsorKeyPool::random_single());
    let policy = SponsorshipPolicy {
        critical_watermark_xlm: 10.0,
        ..Default::default()
    };
    let relayer = FeeBumpRelayer::new(policy, key_pool, pool);
    relayer.register_authorized_hash("AUTH_HASH_CRIT");

    // Sponsor balance 5.0 XLM is below 10.0 critical watermark
    let res = relayer
        .sponsor_and_wrap("AUTH_HASH_CRIT", "ENVELOPE_XDR", "G_WALLET", 1000, 5.0)
        .await;

    assert!(matches!(
        res,
        Err(RelayerError::SponsorshipPausedCriticalBalance { .. })
    ));
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn test_relayer_wallet_budget_exhaustion() {
    let db_path = std::env::temp_dir().join(format!("zenith-relayer-3-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let key_pool = Arc::new(SponsorKeyPool::random_single());
    let policy = SponsorshipPolicy {
        daily_wallet_budget: 10_000, // 10,000 stroops
        max_fee_per_tx: 10_000,
        ..Default::default()
    };
    let relayer = FeeBumpRelayer::new(policy, key_pool, pool);
    relayer.register_authorized_hash("TX_1");
    relayer.register_authorized_hash("TX_2");

    let res1 = relayer
        .sponsor_and_wrap("TX_1", "ENVELOPE_XDR_1", "G_WALLET", 8_000, 100.0)
        .await;
    assert!(res1.is_ok());

    // Second tx requests 5000, which puts total at 13000 > 10000 limit
    let res2 = relayer
        .sponsor_and_wrap("TX_2", "ENVELOPE_XDR_2", "G_WALLET", 5_000, 100.0)
        .await;

    assert!(matches!(
        res2,
        Err(RelayerError::WalletDailyBudgetExceeded { .. })
    ));
    let _ = std::fs::remove_file(db_path);
}
