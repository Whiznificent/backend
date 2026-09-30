mod common;

use common::TestApp;

#[tokio::test]
async fn mutation_endpoints_are_rate_limited_per_ip() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    // add_watchlist is idempotent (ON CONFLICT DO NOTHING), so repeating it
    // rapidly can't fail on its own merits — any non-2xx status here can
    // only come from the rate limiter itself, not application logic.
    let mut statuses = Vec::new();
    for _ in 0..30 {
        let (status, _) = app
            .post_with(
                "/api/v1/watchlist",
                serde_json::json!({ "underlying": "BTC" }),
                Some(&token),
            )
            .await;
        statuses.push(status.as_u16());
    }

    assert!(
        statuses.contains(&429),
        "expected at least one 429 among 30 rapid requests against a burst_size=20 limit, got: {statuses:?}"
    );
}

#[tokio::test]
async fn mutation_rate_limit_is_scoped_per_wallet_not_per_ip() {
    let app = TestApp::spawn().await;
    // Both requests in this test come from the same (fake, fixed)
    // ConnectInfo peer address the test harness injects — if the mutation
    // limiter were still keyed on IP, wallet_b would inherit however much
    // of the shared quota wallet_a had already burned through.
    let wallet_a = app.login().await;
    let wallet_b = app.login().await;

    // Exhaust wallet_a's quota.
    let mut wallet_a_statuses = Vec::new();
    for _ in 0..30 {
        let (status, _) = app
            .post_with(
                "/api/v1/watchlist",
                serde_json::json!({ "underlying": "BTC" }),
                Some(&wallet_a),
            )
            .await;
        wallet_a_statuses.push(status.as_u16());
    }
    assert!(
        wallet_a_statuses.contains(&429),
        "wallet_a should have exhausted its own quota, got: {wallet_a_statuses:?}"
    );

    // wallet_b, from the "same IP", should still have its own fresh quota.
    let (status, _) = app
        .post_with(
            "/api/v1/watchlist",
            serde_json::json!({ "underlying": "ETH" }),
            Some(&wallet_b),
        )
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "wallet_b must not inherit wallet_a's exhausted quota just because they'd share an IP"
    );
}

#[tokio::test]
async fn auth_nonce_is_rate_limited_per_ip() {
    let app = TestApp::spawn().await;

    let mut statuses = Vec::new();
    for i in 0..20 {
        let (status, _) = app
            .post(
                "/api/v1/auth/nonce",
                serde_json::json!({ "wallet_address": format!("not-a-real-address-{i}") }),
            )
            .await;
        statuses.push(status.as_u16());
    }

    assert!(
        statuses.contains(&429),
        "expected at least one 429 among 20 rapid requests against a burst_size=10 limit, got: {statuses:?}"
    );
}

#[tokio::test]
async fn responses_include_standard_rate_limit_headers_and_api_key_tier() {
    let app = TestApp::spawn().await;
    let (status, headers, _) = app
        .get_raw("/api/v1/spot", None, Some(("x-api-key", "test-client-key")))
        .await;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(headers["ratelimit-limit"], "1200");
    assert!(headers.contains_key("ratelimit-remaining"));
    assert!(headers.contains_key("ratelimit-reset"));
}
