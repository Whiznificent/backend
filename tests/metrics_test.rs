mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn metrics_uses_prometheus_exposition_and_reports_operational_metrics() {
    use tracing_subscriber::prelude::*;

    let subscriber = tracing_subscriber::registry().with(
        zenith_backend::metrics::SqlxQueryMetricsLayer::new()
            .with_filter(tracing_subscriber::filter::LevelFilter::TRACE),
    );
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let app = TestApp::spawn().await;
    let _ = app.get("/metrics").await;
    let (status, body) = app.get("/metrics").await;
    assert_eq!(status, StatusCode::OK);
    let exposition = body.as_str().expect("metrics should be plain text");

    for metric in [
        "http_requests_total",
        "http_request_duration_seconds_bucket",
        "zenith_db_pool_connections",
        "zenith_db_pool_idle_connections",
        "zenith_db_queries_total",
        "zenith_db_query_duration_seconds_sum",
        "zenith_background_loop_healthy",
        "zenith_websocket_connections",
        "zenith_positions_opened_total",
        "zenith_positions_closed_total",
        "zenith_premium_volume_total",
        "zenith_collateral_locked",
        "zenith_alert_triggers_total",
        "zenith_rate_limit_rejections_total",
    ] {
        assert!(exposition.contains(metric), "missing metric {metric}");
    }
    let query_count = exposition
        .lines()
        .find_map(|line| {
            line.strip_prefix("zenith_db_queries_total ")
                .and_then(|value| value.parse::<u64>().ok())
        })
        .expect("query counter should have a Prometheus sample");
    assert!(query_count > 0, "SQLx query events should be measured");
}
