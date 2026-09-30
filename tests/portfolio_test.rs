mod common;

#[tokio::test]
async fn test_unified_portfolio_sync_view() {
    let app = common::TestApp::spawn().await;
    let token = app.login().await;

    // Open a paper position
    let (open_status, open_body) = app
        .post_with(
            "/api/v1/positions/open",
            serde_json::json!({
                "underlying": "XLM",
                "strike": 0.12,
                "expiry_days": 14.0,
                "option_type": "call",
                "position_type": "long",
                "contracts": 100.0,
            }),
            Some(&token),
        )
        .await;
    assert_eq!(open_status, axum::http::StatusCode::OK);

    // Fetch unified portfolio (all sources)
    let (status, body) = app.get_with("/api/v1/portfolio?source=all", Some(&token)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(body["total_positions"].as_u64().unwrap() >= 1);
    let positions = body["positions"].as_array().unwrap();

    let onchain_pos = positions.iter().find(|p| p["source"] == "onchain");
    assert!(onchain_pos.is_some());
    assert!(onchain_pos.unwrap()["entry_premium"].is_null()); // Unknown cost basis

    let paper_pos = positions.iter().find(|p| p["source"] == "paper");
    assert!(paper_pos.is_some());
    assert!(paper_pos.unwrap()["entry_premium"].is_number());
}
