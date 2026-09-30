use zenith_backend::models::Position;
use zenith_backend::repo::{AccountRepo, Alert, AlertRepo, PositionRepo, SessionRepo, WatchlistRepo};

#[tokio::test]
async fn test_repository_layer_crud() {
    let db_path = std::env::temp_dir().join(format!("zenith-repo-test-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let wallet = "GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5";

    // AccountRepo
    let acc = AccountRepo::get_or_create(&pool, wallet).await.unwrap();
    assert_eq!(acc.wallet_address, wallet);
    assert_eq!(acc.balance, 100000.0);

    AccountRepo::update_balances(&pool, wallet, -500.0, 500.0).await.unwrap();
    let acc_updated = AccountRepo::get(&pool, wallet).await.unwrap().unwrap();
    assert_eq!(acc_updated.balance, 99500.0);
    assert_eq!(acc_updated.collateral_locked, 500.0);

    // WatchlistRepo
    WatchlistRepo::add(&pool, wallet, "BTC").await.unwrap();
    let watchlist = WatchlistRepo::list_by_wallet(&pool, wallet).await.unwrap();
    assert_eq!(watchlist, vec!["BTC".to_string()]);
    assert!(WatchlistRepo::remove(&pool, wallet, "BTC").await.unwrap());

    // AlertRepo
    let alert = Alert {
        id: "alert_1".into(),
        wallet_address: wallet.into(),
        underlying: "XLM".into(),
        target_price: 0.15,
        direction: "above".into(),
        status: "active".into(),
        created_at: "2026-09-27T12:00:00Z".into(),
        triggered_at: None,
    };
    AlertRepo::insert(&pool, &alert).await.unwrap();
    let alerts = AlertRepo::list_by_wallet(&pool, wallet).await.unwrap();
    assert_eq!(alerts.len(), 1);

    // PositionRepo
    let pos = Position {
        id: "pos_1".into(),
        wallet_address: wallet.into(),
        underlying: "XLM".into(),
        strike: 0.12,
        expiry_days: 7.0,
        option_type: "call".into(),
        position_type: "long".into(),
        contracts: 10.0,
        entry_premium: 0.01,
        entry_spot: 0.11,
        collateral: 0.0,
        status: "open".into(),
        close_premium: None,
        close_spot: None,
        realized_pnl: None,
        opened_at: "2026-09-27T12:00:00Z".into(),
        closed_at: None,
        strategy_id: None,
    };
    PositionRepo::insert(&pool, &pos).await.unwrap();
    let open_positions = PositionRepo::list_open_by_wallet(&pool, wallet).await.unwrap();
    assert_eq!(open_positions.len(), 1);

    assert!(PositionRepo::close(&pool, "pos_1", wallet, 0.015, 0.13, 50.0, "2026-09-27T13:00:00Z").await.unwrap());

    // SessionRepo
    SessionRepo::create_session(&pool, "token_123", wallet, "2099-01-01T00:00:00Z").await.unwrap();
    let session_wallet = SessionRepo::get_wallet_by_token(&pool, "token_123", "2026-09-27T00:00:00Z").await.unwrap();
    assert_eq!(session_wallet.as_deref(), Some(wallet));

    let _ = std::fs::remove_file(db_path);
}
