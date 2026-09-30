mod common;

use common::TestApp;
use serde_json::json;

#[tokio::test]
async fn public_market_graph_is_read_only_and_connected() {
    let app = TestApp::spawn().await;
    let (status, response) = app
        .post(
            "/api/graphql",
            json!({
                "query": "{ market(underlying: \"BTC\") { underlying spot volatility optionChain(expiryDays: 30) { strike call { premium delta } } volatilitySurface { underlying points { strike expiryDays volatility } } } }"
            }),
        )
        .await;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(response["errors"].is_null());
    assert_eq!(response["data"]["market"]["underlying"], "BTC");
    assert!(response["data"]["market"]["optionChain"]
        .as_array()
        .is_some_and(|chain| chain.len() > 1));
    assert!(response["data"]["market"]["volatilitySurface"]["points"]
        .as_array()
        .is_some_and(|points| !points.is_empty()));
}

#[tokio::test]
async fn portfolio_graph_requires_and_uses_the_authenticated_wallet() {
    let app = TestApp::spawn().await;
    let query = json!({
        "query": "{ portfolio { account { walletAddress balance } positions(limit: 10) { totalCount positions { id market { underlying } strategy { strategyId legs { id } } } } strategies { strategyId legs { id } } history { stats { tradeCount totalRealizedPnl } } } }"
    });

    let (status, unauthenticated) = app.post("/api/graphql", query.clone()).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(!unauthenticated["errors"].is_null());

    let token = app.login().await;
    let (status, response) = app.post_with("/api/graphql", query, Some(&token)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(response["errors"].is_null(), "{response}");
    assert!(response["data"]["portfolio"]["account"]["walletAddress"]
        .as_str()
        .is_some());
    assert_eq!(response["data"]["portfolio"]["positions"]["totalCount"], 0);
    assert_eq!(
        response["data"]["portfolio"]["history"]["stats"]["tradeCount"],
        0
    );
}
