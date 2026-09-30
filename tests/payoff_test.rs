mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn straddle_payoff_is_symmetric_v_shape() {
    let app = TestApp::spawn().await;

    let (status, body) = app
        .post(
            "/api/v1/portfolio/payoff",
            serde_json::json!({
                "legs": [
                    { "option_type": "call", "position_type": "long", "strike": 100, "contracts": 1, "premium": 5 },
                    { "option_type": "put", "position_type": "long", "strike": 100, "contracts": 1, "premium": 5 }
                ],
                "lo_spot": 80, "hi_spot": 120, "steps": 4
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["net_premium"].as_f64().unwrap(), 10.0);

    let points = body["points"].as_array().unwrap();
    assert_eq!(points.len(), 5);
    // Breakevens sit at strike +/- total premium; max loss of exactly
    // -10 lands precisely on the strike (spot = 100, the middle point).
    assert_eq!(points[2]["spot"].as_f64().unwrap(), 100.0);
    assert_eq!(points[2]["pnl"].as_f64().unwrap(), -10.0);
    assert_eq!(
        points[0]["pnl"].as_f64().unwrap(),
        points[4]["pnl"].as_f64().unwrap()
    );
}

#[tokio::test]
async fn payoff_rejects_empty_legs() {
    let app = TestApp::spawn().await;
    let (status, body) = app
        .post(
            "/api/v1/portfolio/payoff",
            serde_json::json!({ "legs": [], "lo_spot": 1, "hi_spot": 2 }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["details"]["fields"].as_array().unwrap().iter().any(|entry| entry["field"] == "legs"));
}

#[tokio::test]
async fn payoff_rejects_inverted_spot_range() {
    let app = TestApp::spawn().await;
    let (status, body) = app
        .post(
            "/api/v1/portfolio/payoff",
            serde_json::json!({
                "legs": [{ "option_type": "call", "position_type": "long", "strike": 1, "contracts": 1, "premium": 1 }],
                "lo_spot": 10, "hi_spot": 5
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["details"]["fields"].as_array().unwrap().iter().any(|entry| entry["field"] == "hi_spot"));
}

#[tokio::test]
async fn portfolio_greeks_are_zero_with_no_positions() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    let (status, greeks) = app.get_with("/api/v1/portfolio/greeks", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(greeks["delta"].as_f64().unwrap(), 0.0);
    assert_eq!(greeks["gamma"].as_f64().unwrap(), 0.0);
    assert_eq!(greeks["theta"].as_f64().unwrap(), 0.0);
    assert_eq!(greeks["vega"].as_f64().unwrap(), 0.0);
}

#[tokio::test]
async fn portfolio_greeks_reflect_an_open_long_call() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    app.post_with(
        "/api/v1/positions/open",
        serde_json::json!({
            "underlying": "BTC", "strike": 70000, "expiry_days": 30,
            "option_type": "call", "position_type": "long", "contracts": 1
        }),
        Some(&token),
    )
    .await;

    let (_, greeks) = app.get_with("/api/v1/portfolio/greeks", Some(&token)).await;
    // A long call has positive delta and positive vega.
    assert!(greeks["delta"].as_f64().unwrap() > 0.0);
    assert!(greeks["vega"].as_f64().unwrap() > 0.0);
}
