mod common;

use axum::http::StatusCode;
use common::TestApp;

async fn execute_two_leg_strategy(app: &TestApp, token: &str) -> String {
    let (status, legs) = app
        .post_with(
            "/api/v1/strategies/execute",
            serde_json::json!({
                "legs": [
                    { "underlying": "BTC", "strike": 68000, "expiry_days": 30, "option_type": "call", "position_type": "long", "contracts": 1 },
                    { "underlying": "BTC", "strike": 72000, "expiry_days": 30, "option_type": "call", "position_type": "short", "contracts": 1 }
                ]
            }),
            Some(token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    legs[0]["strategy_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn execute_strategy_rejects_mixed_underlyings() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    let (status, body) = app
        .post_with(
            "/api/v1/strategies/execute",
            serde_json::json!({
                "legs": [
                    { "underlying": "BTC", "strike": 68000, "expiry_days": 30, "option_type": "call", "position_type": "long", "contracts": 1 },
                    { "underlying": "ETH", "strike": 4500, "expiry_days": 30, "option_type": "call", "position_type": "short", "contracts": 1 }
                ]
            }),
            Some(&token),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("same underlying"));

    let (_, positions) = app.get_with("/api/v1/positions", Some(&token)).await;
    assert_eq!(positions.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_strategies_excludes_plain_single_leg_positions() {
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

    let (status, strategies) = app.get_with("/api/v1/strategies", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(strategies.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_strategies_summarizes_an_open_strategy() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let strategy_id = execute_two_leg_strategy(&app, &token).await;

    let (status, strategies) = app.get_with("/api/v1/strategies", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let strategies = strategies.as_array().unwrap();
    assert_eq!(strategies.len(), 1);
    let s = &strategies[0];
    assert_eq!(s["strategy_id"].as_str().unwrap(), strategy_id);
    assert_eq!(s["underlying"].as_str().unwrap(), "BTC");
    assert_eq!(s["leg_count"].as_i64().unwrap(), 2);
    assert_eq!(s["open_leg_count"].as_i64().unwrap(), 2);
    assert_eq!(s["status"].as_str().unwrap(), "open");
    // Spot/vol haven't moved since the legs were opened (no simulator
    // running in tests), so repricing them now must round-trip exactly.
    assert_eq!(s["realized_pnl"].as_f64().unwrap(), 0.0);
    assert_eq!(s["unrealized_pnl"].as_f64().unwrap(), 0.0);
}

#[tokio::test]
async fn get_strategy_returns_every_leg() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let strategy_id = execute_two_leg_strategy(&app, &token).await;

    let (status, detail) = app
        .get_with(&format!("/api/v1/strategies/{strategy_id}"), Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["strategy_id"].as_str().unwrap(), strategy_id);
    assert_eq!(detail["status"].as_str().unwrap(), "open");
    assert_eq!(detail["legs"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn get_strategy_404s_for_an_unknown_id() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    let (status, _) = app
        .get_with("/api/v1/strategies/does-not-exist", Some(&token))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_strategies_keeps_status_open_after_a_partial_close() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let strategy_id = execute_two_leg_strategy(&app, &token).await;

    let (_, detail) = app
        .get_with(&format!("/api/v1/strategies/{strategy_id}"), Some(&token))
        .await;
    let leg_id = detail["legs"][0]["id"].as_str().unwrap().to_string();

    let (status, _) = app
        .post_with(
            &format!("/api/v1/positions/{leg_id}/close"),
            serde_json::Value::Null,
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, strategies) = app.get_with("/api/v1/strategies", Some(&token)).await;
    let s = &strategies[0];
    assert_eq!(
        s["status"].as_str().unwrap(),
        "open",
        "one leg still open must keep the whole strategy 'open'"
    );
    assert_eq!(s["leg_count"].as_i64().unwrap(), 2);
    assert_eq!(s["open_leg_count"].as_i64().unwrap(), 1);
}

#[tokio::test]
async fn close_strategy_closes_every_open_leg_atomically() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let strategy_id = execute_two_leg_strategy(&app, &token).await;

    let (status, closed) = app
        .post_with(
            &format!("/api/v1/strategies/{strategy_id}/close"),
            serde_json::Value::Null,
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let closed = closed.as_array().unwrap();
    assert_eq!(closed.len(), 2);
    assert!(closed.iter().all(|leg| leg["status"] == "closed"));

    let (_, detail) = app
        .get_with(&format!("/api/v1/strategies/{strategy_id}"), Some(&token))
        .await;
    assert_eq!(detail["status"].as_str().unwrap(), "closed");

    let (_, summaries) = app.get_with("/api/v1/strategies", Some(&token)).await;
    assert_eq!(summaries[0]["open_leg_count"].as_i64().unwrap(), 0);
    assert_eq!(summaries[0]["status"].as_str().unwrap(), "closed");
}

#[tokio::test]
async fn close_strategy_does_not_reclose_a_leg_already_settled_by_a_manual_close() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let strategy_id = execute_two_leg_strategy(&app, &token).await;

    let (_, detail) = app
        .get_with(&format!("/api/v1/strategies/{strategy_id}"), Some(&token))
        .await;
    let manually_closed_leg = detail["legs"][0]["id"].as_str().unwrap().to_string();

    let (status, _) = app
        .post_with(
            &format!("/api/v1/positions/{manually_closed_leg}/close"),
            serde_json::Value::Null,
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, closed) = app
        .post_with(
            &format!("/api/v1/strategies/{strategy_id}/close"),
            serde_json::Value::Null,
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let closed = closed.as_array().unwrap();
    assert_eq!(
        closed.len(),
        1,
        "only the leg still open should be settled by close_strategy"
    );
    assert_ne!(
        closed[0]["id"].as_str().unwrap(),
        manually_closed_leg,
        "the already-closed leg must not be settled a second time"
    );
}

#[tokio::test]
async fn closing_an_already_closed_strategy_404s() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let strategy_id = execute_two_leg_strategy(&app, &token).await;

    let (first, _) = app
        .post_with(
            &format!("/api/v1/strategies/{strategy_id}/close"),
            serde_json::Value::Null,
            Some(&token),
        )
        .await;
    assert_eq!(first, StatusCode::OK);

    let (second, _) = app
        .post_with(
            &format!("/api/v1/strategies/{strategy_id}/close"),
            serde_json::Value::Null,
            Some(&token),
        )
        .await;
    assert_eq!(second, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn strangers_cannot_view_or_close_someone_elses_strategy() {
    let app = TestApp::spawn().await;
    let owner = app.login().await;
    let stranger = app.login().await;
    let strategy_id = execute_two_leg_strategy(&app, &owner).await;

    let (status, _) = app
        .get_with(
            &format!("/api/v1/strategies/{strategy_id}"),
            Some(&stranger),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = app
        .post_with(
            &format!("/api/v1/strategies/{strategy_id}/close"),
            serde_json::Value::Null,
            Some(&stranger),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The owner's strategy must be untouched by the stranger's failed attempt.
    let (_, detail) = app
        .get_with(&format!("/api/v1/strategies/{strategy_id}"), Some(&owner))
        .await;
    assert_eq!(detail["status"].as_str().unwrap(), "open");

    // And it must not even show up in the stranger's own strategy list.
    let (_, stranger_list) = app.get_with("/api/v1/strategies", Some(&stranger)).await;
    assert_eq!(stranger_list.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn roll_keeps_the_replacement_leg_closable_via_the_strategy() {
    let app = TestApp::spawn().await;
    let token = app.login().await;
    let strategy_id = execute_two_leg_strategy(&app, &token).await;

    let (_, detail) = app
        .get_with(&format!("/api/v1/strategies/{strategy_id}"), Some(&token))
        .await;
    let leg_id = detail["legs"][0]["id"].as_str().unwrap().to_string();

    let (status, _) = app
        .post_with(
            &format!("/api/v1/positions/{leg_id}/roll"),
            serde_json::json!({ "new_strike": 69000, "new_expiry_days": 45 }),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 3 legs total now (1 rolled + 1 untouched + 1 replacement), still one
    // strategy, still 2 open legs.
    let (_, detail) = app
        .get_with(&format!("/api/v1/strategies/{strategy_id}"), Some(&token))
        .await;
    assert_eq!(detail["legs"].as_array().unwrap().len(), 3);
    assert_eq!(detail["status"].as_str().unwrap(), "open");

    let (status, closed) = app
        .post_with(
            &format!("/api/v1/strategies/{strategy_id}/close"),
            serde_json::Value::Null,
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        closed.as_array().unwrap().len(),
        2,
        "closes the 2 still-open legs (the rolled leg is already settled)"
    );
}
