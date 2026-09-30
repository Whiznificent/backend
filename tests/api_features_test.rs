mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn batch_prices_independent_items_against_one_request() {
    let app = TestApp::spawn().await;
    let (status, body) = app
        .post(
            "/api/v1/price/batch",
            serde_json::json!({
                "specs": [
                    {"underlying":"BTC","strike":67000.0,"expiry_days":30.0,"option_type":"call"},
                    {"underlying":"UNKNOWN","strike":100.0,"expiry_days":30.0,"option_type":"put"}
                ],
                "strategies": [
                    {"id":"vertical","legs":[
                        {"underlying":"ETH","strike":3500.0,"expiry_days":14.0,"option_type":"call"},
                        {"underlying":"SOL","strike":180.0,"expiry_days":14.0,"option_type":"bad"}
                    ]}
                ]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["specs"][0]["result"]["premium"].as_f64().unwrap() > 0.0);
    assert_eq!(body["specs"][1]["error"]["code"], "unknown_underlying");
    assert!(body["strategies"][0]["legs"][0]["result"]["premium"].as_f64().is_some());
    assert_eq!(body["strategies"][0]["legs"][1]["error"]["code"], "invalid_option_type");
}

#[tokio::test]
async fn batch_rejects_more_than_500_option_specs() {
    let app = TestApp::spawn().await;
    let spec = serde_json::json!({
        "underlying":"BTC","strike":67000.0,"expiry_days":30.0,"option_type":"call"
    });
    let specs = (0..501).map(|_| spec.clone()).collect::<Vec<_>>();
    let (status, body) = app
        .post("/api/v1/price/batch", serde_json::json!({"specs": specs}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["details"]["fields"].as_array().is_some_and(|fields| !fields.is_empty()));
}

#[tokio::test]
async fn session_can_create_an_hmac_key_that_prices_with_read_scope() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let (status, key) = app
        .post_with(
            "/api/v1/auth/keys",
            serde_json::json!({"label":"market dashboard","scopes":["read"]}),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, response) = app
        .signed_post(
            "/api/v1/price/batch",
            serde_json::json!({"specs":[
                {"underlying":"BTC","strike":67000.0,"expiry_days":30.0,"option_type":"call"}
            ]}),
            key["id"].as_str().unwrap(),
            key["secret"].as_str().unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(response["specs"][0]["result"]["premium"].as_f64().is_some());
}

#[tokio::test]
async fn validation_uses_422_and_returns_field_errors() {
    let app = TestApp::spawn().await;
    let (status, body) = app
        .post(
            "/api/v1/portfolio/payoff",
            serde_json::json!({"legs":[],"lo_spot":100.0,"hi_spot":90.0,"steps":0}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let fields = body["details"]["fields"].as_array().unwrap();
    assert!(fields.iter().any(|entry| entry["field"] == "legs"));
}

#[tokio::test]
async fn read_responses_have_strong_etags_and_honor_if_none_match() {
    let app = TestApp::spawn().await;
    let (status, headers, _) = app.get_raw("/api/v1/spot", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("cache-control").unwrap(),
        "public, max-age=1, must-revalidate"
    );
    let etag = headers.get("etag").unwrap().to_str().unwrap();
    assert!(etag.starts_with('"') && etag.ends_with('"'));

    let (status, _, _) = app
        .get_raw("/api/v1/spot", None, Some(("if-none-match", etag)))
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn large_market_responses_are_compressed() {
    let app = TestApp::spawn().await;
    let (status, headers, _) = app
        .get_raw(
            "/api/v1/chain?underlying=BTC&expiry_days=30",
            None,
            Some(("accept-encoding", "br")),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("content-encoding").unwrap(), "br");
}
