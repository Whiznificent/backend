mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

#[tokio::test]
async fn api_keys_are_wallet_scoped_and_revocable() {
    let app = TestApp::spawn().await;
    let owner = app.login().await;
    let stranger = app.login().await;

    let (status, key) = app
        .post_with(
            "/api/v1/delivery/api-keys",
            json!({"name":"automation"}),
            Some(&owner),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(key["key"].as_str().unwrap().starts_with("znt_"));
    let id = key["id"].as_str().unwrap();

    let (status, _) = app
        .delete_with(&format!("/api/v1/delivery/api-keys/{id}"), &stranger)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = app
        .delete_with(&format!("/api/v1/delivery/api-keys/{id}"), &owner)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delivery_logs_require_wallet_authentication() {
    let app = TestApp::spawn().await;
    let (status, _) = app.get("/api/v1/delivery/logs").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
