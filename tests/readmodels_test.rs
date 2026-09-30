mod common;

use axum::http::StatusCode;
use common::TestApp;

/// Seeds one wallet with 10k positions (a mix of single-leg trades and
/// multi-leg strategies, open and closed) directly via SQL, rebuilds the
/// read models, then asserts the read-model serving queries stay under the
/// 10ms p99 bar the issue sets for a wallet of this size.
#[tokio::test]
async fn read_model_queries_stay_under_10ms_p99_with_10k_positions() {
    let app = TestApp::spawn().await;
    let db = app.db();

    sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('WALLET1')")
        .execute(db)
        .await
        .unwrap();

    // 10k positions: every 10th row joins one of 1000 strategies (10 legs
    // each), the rest are plain single-leg trades. A third of all rows are
    // still open; closed rows carry a spread of winning/losing/break-even
    // realized P&L.
    sqlx::query(
        "WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM cnt WHERE x < 10000)
         INSERT INTO positions
             (id, wallet_address, underlying, strike, expiry_days, option_type,
              position_type, contracts, entry_premium, entry_spot, status, realized_pnl, strategy_id)
         SELECT 'pos-' || x,
                'WALLET1',
                'BTC',
                70000,
                30,
                'call',
                'long',
                1,
                100,
                67000,
                CASE WHEN x % 3 = 0 THEN 'open' ELSE 'closed' END,
                CASE WHEN x % 3 = 0 THEN NULL ELSE (x % 7) - 3 END,
                CASE WHEN x % 10 = 0 THEN 'strat-' || (x / 10) ELSE NULL END
           FROM cnt",
    )
    .execute(db)
    .await
    .unwrap();

    zenith_backend::readmodels::rebuild_all(db).await.unwrap();

    let strategies =
        zenith_backend::readmodels::strategy_summaries_for_wallet(db, "WALLET1")
            .await
            .unwrap();
    assert_eq!(strategies.len(), 1000);
    let counts = zenith_backend::readmodels::wallet_position_counts(db, "WALLET1")
        .await
        .unwrap()
        .expect("10k seeded positions must produce a counts row");
    // 3333 of 10k rows are open (x % 3 = 0), so 6667 are closed; of those,
    // realized_pnl = (x % 7) - 3.
    assert_eq!(counts.trade_count, 6667);
    assert!(counts.win_count > 0);
    assert!(counts.loss_count > 0);

    // Warm up (pool, page cache) before measuring.
    for _ in 0..20 {
        let _ = zenith_backend::readmodels::strategy_summaries_for_wallet(db, "WALLET1")
            .await
            .unwrap();
        let _ = zenith_backend::readmodels::wallet_position_counts(db, "WALLET1")
            .await
            .unwrap();
    }

    let mut durations = Vec::with_capacity(200);
    for _ in 0..200 {
        let start = std::time::Instant::now();
        let _ = zenith_backend::readmodels::strategy_summaries_for_wallet(db, "WALLET1")
            .await
            .unwrap();
        let _ = zenith_backend::readmodels::wallet_position_counts(db, "WALLET1")
            .await
            .unwrap();
        durations.push(start.elapsed());
    }
    durations.sort();
    let p99 = durations[(durations.len() as f64 * 0.99) as usize - 1];
    assert!(
        p99 < std::time::Duration::from_millis(10),
        "read-model queries p99 {p99:?} exceeded 10ms with 10k positions"
    );
}

/// End-to-end through the HTTP API: the read models back both
/// list_strategies and the history stats, maintained in the same
/// transaction as each open/close.
#[tokio::test]
async fn list_strategies_and_history_stats_are_served_from_the_read_models() {
    let app = TestApp::spawn().await;
    let token = app.login().await;

    // Two single-leg trades: one closed at a win, one still open.
    let (_, opened) = app
        .post_with(
            "/api/v1/positions/open",
            serde_json::json!({
                "underlying": "BTC", "strike": 70000, "expiry_days": 30,
                "option_type": "call", "position_type": "long", "contracts": 1
            }),
            Some(&token),
        )
        .await;
    let win_id = opened["id"].as_str().unwrap().to_string();
    app.post_with(
        &format!("/api/v1/positions/{win_id}/close"),
        serde_json::Value::Null,
        Some(&token),
    )
    .await;

    let (_, opened) = app
        .post_with(
            "/api/v1/positions/open",
            serde_json::json!({
                "underlying": "ETH", "strike": 3500, "expiry_days": 30,
                "option_type": "call", "position_type": "long", "contracts": 1
            }),
            Some(&token),
        )
        .await;
    let _open_id = opened["id"].as_str().unwrap().to_string();

    // A two-leg strategy, one leg closed.
    let (_, legs) = app
        .post_with(
            "/api/v1/strategies/execute",
            serde_json::json!({
                "legs": [
                    { "underlying": "BTC", "strike": 68000, "expiry_days": 30, "option_type": "call", "position_type": "long", "contracts": 1 },
                    { "underlying": "BTC", "strike": 72000, "expiry_days": 30, "option_type": "call", "position_type": "short", "contracts": 1 }
                ]
            }),
            Some(&token),
        )
        .await;
    let strategy_id = legs[0]["strategy_id"].as_str().unwrap().to_string();
    let leg0_id = legs[0]["id"].as_str().unwrap().to_string();
    app.post_with(
        &format!("/api/v1/positions/{leg0_id}/close"),
        serde_json::Value::Null,
        Some(&token),
    )
    .await;

    // list_strategies: one strategy, one leg still open, realized P&L from
    // the read model.
    let (status, strategies) = app.get_with("/api/v1/strategies", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let strategies = strategies.as_array().unwrap();
    assert_eq!(strategies.len(), 1);
    let s = &strategies[0];
    assert_eq!(s["strategy_id"].as_str().unwrap(), strategy_id);
    assert_eq!(s["leg_count"].as_i64().unwrap(), 2);
    assert_eq!(s["open_leg_count"].as_i64().unwrap(), 1);
    assert_eq!(s["status"].as_str().unwrap(), "open");

    // history stats: two settled trades (the single-leg win + the closed
    // strategy leg), served from wallet_position_counts.
    let (status, history) = app.get_with("/api/v1/history", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history["stats"]["trade_count"].as_i64().unwrap(), 2);
    assert!(history["stats"]["win_count"].as_i64().unwrap() >= 1);
    assert!(
        history["stats"]["total_realized_pnl"]
            .as_f64()
            .unwrap()
            .abs()
            < 1e-6,
        "spot/vol never moved in tests, so every close settles at entry premium"
    );
}
