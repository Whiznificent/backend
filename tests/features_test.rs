mod common;

use common::TestApp;

#[tokio::test]
async fn feature_catalog_is_public_and_defaults_to_empty() {
    let app = TestApp::spawn().await;
    let (status, body) = app.get("/api/v1/features").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(body["environment"].as_str().is_some());
    assert_eq!(body["features"].as_object().unwrap().len(), 0);
}
