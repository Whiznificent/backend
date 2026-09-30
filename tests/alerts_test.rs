mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn create_list_delete_round_trip() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    let (status, alert) = app
        .post_with(
            "/api/v1/alerts",
            serde_json::json!({ "underlying": "BTC", "condition": "above", "target_price": 70000 }),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!alert["triggered"].as_bool().unwrap());
    let id = alert["id"].as_str().unwrap().to_string();

    let (_, list) = app.get_with("/api/v1/alerts", Some(&token)).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, _) = app
        .delete_with(&format!("/api/v1/alerts/{id}"), &token)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, list_after) = app.get_with("/api/v1/alerts", Some(&token)).await;
    assert_eq!(list_after.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn rejects_invalid_condition() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    let (status, body) = app
        .post_with(
            "/api/v1/alerts",
            serde_json::json!({ "underlying": "BTC", "condition": "sideways", "target_price": 1 }),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("above"));
}

#[tokio::test]
async fn rejects_non_positive_target_price() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    let (status, _) = app
        .post_with(
            "/api/v1/alerts",
            serde_json::json!({ "underlying": "BTC", "condition": "above", "target_price": 0 }),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_unknown_underlying() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    let (status, _) = app
        .post_with(
            "/api/v1/alerts",
            serde_json::json!({ "underlying": "DOGE", "condition": "above", "target_price": 1 }),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_someone_elses_alert_404s() {
    let app = TestApp::spawn().await;
    let owner = app.login().await;
    let stranger = app.login().await;

    let (_, alert) = app
        .post_with(
            "/api/v1/alerts",
            serde_json::json!({ "underlying": "BTC", "condition": "above", "target_price": 1 }),
            Some(&owner),
        )
        .await;
    let id = alert["id"].as_str().unwrap();

    let (status, _) = app
        .delete_with(&format!("/api/v1/alerts/{id}"), &stranger)
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a wallet must not be able to delete another wallet's alert"
    );
}

#[tokio::test]
async fn portfolio_delta_band_does_not_require_an_underlying_or_target_price() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    let (status, alert) = app
        .post_with(
            "/api/v1/alerts",
            serde_json::json!({
                "condition": "portfolio_delta_outside",
                "lower_bound": -10.0,
                "upper_bound": 10.0
            }),
            Some(&token),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{alert}");
    assert_eq!(alert["underlying"], "PORTFOLIO");
    assert_eq!(alert["lower_bound"], -10.0);
    assert_eq!(alert["upper_bound"], 10.0);
}

#[tokio::test]
async fn position_pnl_alert_derives_its_underlying_from_the_owned_position() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let (_, position) = app
        .post_with(
            "/api/v1/positions/open",
            serde_json::json!({
                "underlying": "BTC",
                "strike": 70000,
                "expiry_days": 30,
                "option_type": "call",
                "position_type": "long",
                "contracts": 1
            }),
            Some(&token),
        )
        .await;

    let (status, alert) = app
        .post_with(
            "/api/v1/alerts",
            serde_json::json!({
                "condition": "position_pnl_above",
                "target_price": 100.0,
                "position_id": position["id"]
            }),
            Some(&token),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{alert}");
    assert_eq!(alert["underlying"], "BTC");
}
