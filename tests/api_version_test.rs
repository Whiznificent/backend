mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn v1_and_v2_are_available_with_their_respective_spot_dtos() {
    let app = TestApp::spawn().await;
    let (v1_status, v1) = app.get("/api/v1/spot").await;
    let (v2_status, v2) = app.get("/api/v2/spot").await;

    assert_eq!(v1_status, StatusCode::OK);
    assert_eq!(v2_status, StatusCode::OK);
    assert!(v1["prices"]["XLM"].is_number());
    assert!(v1["vols"]["XLM"].is_number());
    assert!(v2["assets"]["XLM"]["price"].is_number());
    assert!(v2["assets"]["XLM"]["implied_vol"].is_number());

    let (usage_status, usage) = app.get("/api/versions/usage").await;
    assert_eq!(usage_status, StatusCode::OK);
    assert!(usage["v1"].as_u64().unwrap() >= 1);
    assert!(usage["v2"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn v2_errors_are_structured_and_v1_stays_compatible_by_default() {
    let app = TestApp::spawn().await;
    let (v2_status, headers, v2) = app
        .get_raw(
            "/api/v2/price?underlying=UNKNOWN&strike=1&expiry_days=1&option_type=call",
            None,
            None,
        )
        .await;
    assert_eq!(v2_status, StatusCode::NOT_FOUND);
    assert_eq!(v2["error"]["code"], "NOT_FOUND");
    assert_eq!(
        v2["error"]["request_id"],
        headers["x-request-id"].to_str().unwrap()
    );

    let (v1_status, v1) = app
        .get("/api/v1/price?underlying=UNKNOWN&strike=1&expiry_days=1&option_type=call")
        .await;
    assert_eq!(v1_status, StatusCode::NOT_FOUND);
    assert!(v1["error"].is_string());
}

#[tokio::test]
async fn deprecated_v1_route_receives_standard_deprecation_headers() {
    let config = zenith_backend::config::Config {
        deprecated_routes: vec!["/api/v1/spot".into()],
        deprecation_timestamp: Some(1_800_000_000),
        sunset_date: Some("Tue, 28 Sep 2027 00:00:00 GMT".into()),
        ..zenith_backend::config::Config::default()
    };
    config.validate().unwrap();
    let app = TestApp::spawn_with_config(config).await;

    let (_, headers, _) = app.get_raw("/api/v1/spot", None, None).await;
    assert_eq!(headers["deprecation"], "@1800000000");
    assert_eq!(headers["sunset"], "Tue, 28 Sep 2027 00:00:00 GMT");

    let (_, v2_headers, _) = app.get_raw("/api/v2/spot", None, None).await;
    assert!(!v2_headers.contains_key("deprecation"));
    assert!(!v2_headers.contains_key("sunset"));
}
