mod common;

use std::sync::Arc;
use zenith_backend::config::{NetworkConfig, NetworkConfigError, NetworkType};
use zenith_backend::AppState;

#[tokio::test]
async fn test_health_reports_configured_network() {
    let app = common::TestApp::spawn().await;
    let (status, body) = app.get("/health").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["network"], "testnet");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn test_stats_reports_configured_network() {
    let app = common::TestApp::spawn().await;
    let (status, body) = app.get("/api/v1/stats").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["network"], "testnet");
}

#[tokio::test]
async fn test_futurenet_network_routing() {
    let db_path = std::env::temp_dir().join(format!("zenith-fn-test-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let futurenet_cfg = Arc::new(NetworkConfig::futurenet());
    futurenet_cfg.validate_db_network(&pool).await.unwrap();

    let state = AppState::new_with_network(pool, futurenet_cfg);
    let router = zenith_backend::build_router(state);

    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap();

    let response = router.oneshot(req).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["network"], "futurenet");

    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn test_db_network_mismatch_refusal() {
    let db_path = std::env::temp_dir().join(format!("zenith-mismatch-test-{}.db", uuid::Uuid::new_v4()));
    let database_url = format!("sqlite://{}", db_path.display());
    let pool = zenith_backend::db::init_pool(&database_url).await;

    let testnet_cfg = NetworkConfig::testnet();
    testnet_cfg.validate_db_network(&pool).await.unwrap();

    let mainnet_cfg = NetworkConfig::mainnet();
    let res = mainnet_cfg.validate_db_network(&pool).await;
    assert!(matches!(res, Err(NetworkConfigError::DatabaseNetworkMismatch { .. })));

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn test_passphrase_validation_mismatch() {
    let config = NetworkConfig::for_network(NetworkType::Testnet);
    let result = config.validate_passphrase("Wrong Passphrase Network");
    assert!(matches!(result, Err(NetworkConfigError::PassphraseMismatch { .. })));
}
