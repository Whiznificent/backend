mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn livez_only_reports_that_the_process_is_alive() {
    let app = TestApp::spawn().await;
    let (status, body) = app.get("/livez").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

#[tokio::test]
async fn readyz_checks_database_migrations_market_data_and_drain_state() {
    let app = TestApp::spawn().await;
    let (status, body) = app.get("/readyz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ready");
    for dependency in ["database", "migrations", "market_data", "draining"] {
        assert_eq!(body["dependencies"][dependency]["status"], "ok");
    }
}

#[tokio::test]
async fn health_details_requires_authentication_and_reports_dependency_latency() {
    let app = TestApp::spawn().await;
    let (status, _) = app.get("/health/details").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let token = app.login().await;
    let (status, body) = app.get_with("/health/details", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["dependencies"]["database"]["status"], "ok");
    assert!(body["dependencies"]["database"]["latency_ms"].is_number());
    assert_eq!(
        body["dependencies"]["database"]["last_error"],
        serde_json::Value::Null
    );
    assert_eq!(body["dependencies"]["migrations"]["status"], "ok");
    assert_eq!(body["dependencies"]["market_data"]["status"], "ok");
    assert_eq!(body["dependencies"]["draining"]["status"], "ok");
}
