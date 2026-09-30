mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn a_request_without_one_gets_a_fresh_request_id_assigned() {
    let app = TestApp::spawn().await;
    let (status, headers, _) = app.get_raw("/livez", None, None).await;
    assert_eq!(status, StatusCode::OK);

    let id = headers
        .get("x-request-id")
        .expect("response should carry an x-request-id header")
        .to_str()
        .unwrap();
    // A UUIDv4 string, not just any non-empty value.
    assert_eq!(id.len(), 36);
    assert_eq!(id.matches('-').count(), 4);
}

#[tokio::test]
async fn a_client_supplied_request_id_is_echoed_back_unchanged() {
    let app = TestApp::spawn().await;
    let (status, headers, _) = app
        .get_raw("/livez", None, Some(("x-request-id", "my-own-id-123")))
        .await;
    assert_eq!(status, StatusCode::OK);

    let id = headers.get("x-request-id").unwrap().to_str().unwrap();
    assert_eq!(id, "my-own-id-123");
}

#[tokio::test]
async fn two_separate_requests_get_different_ids() {
    let app = TestApp::spawn().await;
    let (_, headers1, _) = app.get_raw("/livez", None, None).await;
    let (_, headers2, _) = app.get_raw("/livez", None, None).await;

    let id1 = headers1.get("x-request-id").unwrap().to_str().unwrap();
    let id2 = headers2.get("x-request-id").unwrap().to_str().unwrap();
    assert_ne!(id1, id2);
}

#[tokio::test]
async fn structured_v1_compatibility_error_carries_the_response_request_id() {
    let app = TestApp::spawn().await;
    let (status, headers, body) = app
        .get_raw(
            "/api/v1/price?underlying=UNKNOWN&strike=1&expiry_days=1&option_type=call",
            None,
            Some(("x-api-error-format", "structured")),
        )
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    let response_id = headers.get("x-request-id").unwrap().to_str().unwrap();
    assert_eq!(body["error"]["code"], "NOT_FOUND");
    assert_eq!(body["error"]["message"], "Not Found");
    assert_eq!(body["error"]["details"], serde_json::json!({}));
    assert_eq!(body["error"]["request_id"], response_id);
}
