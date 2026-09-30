mod common;

use common::TestApp;
use serde::Deserialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[tokio::test]
async fn serves_openapi_json_and_swagger_ui() {
    let app = TestApp::spawn().await;

    let (status, spec) = app.get("/api/v1/openapi.json").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(spec["openapi"], "3.1.0");
    for path in [
        "/health",
        "/api/v1/spot",
        "/api/v1/price",
        "/api/v1/iv",
        "/api/v1/chain",
        "/api/v1/expiries/{underlying}",
        "/api/v1/stats",
        "/api/v1/auth/nonce",
        "/api/v1/auth/verify",
        "/api/v1/auth/me",
        "/api/v1/account",
        "/api/v1/positions",
        "/api/v1/positions/open",
        "/api/v1/positions/{id}/close",
        "/api/v1/positions/{id}/roll",
        "/api/v1/history",
        "/api/v1/watchlist",
        "/api/v1/watchlist/{underlying}",
        "/api/v1/alerts",
        "/api/v1/alerts/{id}",
        "/api/v1/strategies/execute",
        "/api/v1/strategies",
        "/api/v1/strategies/{id}",
        "/api/v1/strategies/{id}/close",
        "/api/v1/ws/spot",
        "/api/v1/portfolio/payoff",
        "/api/v1/portfolio/greeks",
        "/api/v1/openapi.json",
    ] {
        assert!(spec["paths"][path].is_object(), "missing path {path}");
    }

    let (status, headers, _) = app.get_raw("/docs/", None, None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(headers["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
}

#[tokio::test]
async fn timestamp_response_fields_use_rfc3339() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let (status, account) = app.get_with("/api/v1/account", Some(&token)).await;

    assert_eq!(status, axum::http::StatusCode::OK);
    let timestamp = account["created_at"].as_str().unwrap();
    assert!(timestamp.contains('T'));
    assert!(OffsetDateTime::parse(timestamp, &Rfc3339).is_ok());

    let schema: Value = serde_json::from_str(include_str!("../openapi.json")).unwrap();
    assert_eq!(
        schema["components"]["schemas"]["Account"]["properties"]["created_at"]["format"],
        "date-time"
    );

    #[derive(Deserialize)]
    struct StrictTimestamp {
        #[serde(with = "time::serde::rfc3339")]
        _timestamp: OffsetDateTime,
    }
    assert!(
        serde_json::from_value::<StrictTimestamp>(serde_json::json!({
            "_timestamp": "2026-09-28 21:38:00"
        }))
        .is_err()
    );
}
