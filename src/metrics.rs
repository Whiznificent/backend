use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use crate::AppState;

const DURATION_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Default)]
struct HttpSeries {
    count: u64,
    duration_sum: f64,
    buckets: [u64; DURATION_BUCKETS.len()],
}

#[derive(Default)]
pub struct Metrics {
    http: Mutex<BTreeMap<(String, String, String), HttpSeries>>,
    positions_opened: AtomicU64,
    positions_closed: AtomicU64,
    premium_volume: Mutex<f64>,
    collateral_locked: Mutex<f64>,
    alert_triggers: AtomicU64,
    rate_limit_rejections: AtomicU64,
    websocket_connections: AtomicU64,
    db_query_count: AtomicU64,
    db_query_duration_seconds: Mutex<f64>,
}

static GLOBAL_METRICS: OnceLock<Arc<Metrics>> = OnceLock::new();

pub fn global() -> Arc<Metrics> {
    GLOBAL_METRICS
        .get_or_init(|| Arc::new(Metrics::default()))
        .clone()
}

pub async fn request_metrics(request: Request<Body>, next: Next) -> Response {
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "/_unmatched".to_owned());
    let started = Instant::now();
    let response = next.run(request).await;
    global().record_http(
        &method,
        &route,
        response.status().as_u16(),
        started.elapsed(),
    );
    response
}

impl Metrics {
    pub fn record_http(&self, method: &str, route: &str, status: u16, duration: Duration) {
        let status_class = format!("{}xx", status / 100);
        let seconds = duration.as_secs_f64();
        let mut http = self.http.lock().unwrap();
        let series = http
            .entry((method.to_owned(), route.to_owned(), status_class))
            .or_default();
        series.count += 1;
        series.duration_sum += seconds;
        for (index, bucket) in DURATION_BUCKETS.iter().enumerate() {
            if seconds <= *bucket {
                series.buckets[index] += 1;
            }
        }
        if status == 429 {
            self.rate_limit_rejections.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_database_query(&self, duration: Option<Duration>) {
        self.db_query_count.fetch_add(1, Ordering::Relaxed);
        if let Some(duration) = duration {
            *self.db_query_duration_seconds.lock().unwrap() += duration.as_secs_f64();
        }
    }

    pub fn record_position_opened(&self, premium: f64, collateral: f64) {
        self.positions_opened.fetch_add(1, Ordering::Relaxed);
        *self.premium_volume.lock().unwrap() += premium;
        *self.collateral_locked.lock().unwrap() += collateral;
    }

    pub fn record_position_closed(&self, premium: f64, collateral: f64) {
        self.positions_closed.fetch_add(1, Ordering::Relaxed);
        *self.premium_volume.lock().unwrap() += premium;
        let mut locked = self.collateral_locked.lock().unwrap();
        *locked = (*locked - collateral).max(0.0);
    }

    pub fn set_collateral_locked(&self, amount: f64) {
        *self.collateral_locked.lock().unwrap() = amount.max(0.0);
    }

    pub fn record_alert_triggers(&self, count: u64) {
        self.alert_triggers.fetch_add(count, Ordering::Relaxed);
    }

    pub fn websocket_connected(self: &Arc<Self>) -> WebSocketConnection {
        self.websocket_connections.fetch_add(1, Ordering::Relaxed);
        WebSocketConnection(self.clone())
    }

    fn exposition(
        &self,
        pool_size: u32,
        pool_idle: usize,
        loop_health: impl Iterator<Item = (&'static str, bool)>,
    ) -> String {
        let mut output = String::from(
            "# HELP http_requests_total HTTP requests by route, method, and status class.\n\
             # TYPE http_requests_total counter\n\
             # HELP http_request_duration_seconds HTTP request duration in seconds.\n\
             # TYPE http_request_duration_seconds histogram\n",
        );
        let http = self.http.lock().unwrap();
        for ((method, route, status_class), series) in http.iter() {
            let labels = format!(
                "method=\"{}\",route=\"{}\",status_class=\"{}\"",
                escape_label(method),
                escape_label(route),
                escape_label(status_class)
            );
            output.push_str(&format!(
                "http_requests_total{{{labels}}} {}\n",
                series.count
            ));
            for (index, bucket) in DURATION_BUCKETS.iter().enumerate() {
                output.push_str(&format!(
                    "http_request_duration_seconds_bucket{{{labels},le=\"{bucket}\"}} {}\n",
                    series.buckets[index]
                ));
            }
            output.push_str(&format!(
                "http_request_duration_seconds_bucket{{{labels},le=\"+Inf\"}} {}\n\
                 http_request_duration_seconds_sum{{{labels}}} {}\n\
                 http_request_duration_seconds_count{{{labels}}} {}\n",
                series.count, series.duration_sum, series.count
            ));
        }
        drop(http);

        output.push_str(
            "# HELP zenith_db_pool_connections Current SQLite pool connections.\n\
             # TYPE zenith_db_pool_connections gauge\n",
        );
        output.push_str(&format!("zenith_db_pool_connections {pool_size}\n"));
        output.push_str(
            "# HELP zenith_db_pool_idle_connections Current idle SQLite pool connections.\n\
             # TYPE zenith_db_pool_idle_connections gauge\n",
        );
        output.push_str(&format!("zenith_db_pool_idle_connections {pool_idle}\n"));
        output.push_str(
            "# HELP zenith_db_queries_total Completed SQLx query spans.\n\
             # TYPE zenith_db_queries_total counter\n",
        );
        output.push_str(&format!(
            "zenith_db_queries_total {}\n",
            self.db_query_count.load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP zenith_db_query_duration_seconds_sum Total completed SQLx query duration.\n\
             # TYPE zenith_db_query_duration_seconds_sum counter\n",
        );
        output.push_str(&format!(
            "zenith_db_query_duration_seconds_sum {}\n",
            *self.db_query_duration_seconds.lock().unwrap()
        ));

        output.push_str(
            "# HELP zenith_background_loop_healthy Whether a background loop completed recently.\n\
             # TYPE zenith_background_loop_healthy gauge\n",
        );
        for (name, healthy) in loop_health {
            output.push_str(&format!(
                "zenith_background_loop_healthy{{name=\"{name}\"}} {}\n",
                u8::from(healthy)
            ));
        }

        output.push_str(
            "# HELP zenith_websocket_connections Active WebSocket connections.\n\
             # TYPE zenith_websocket_connections gauge\n",
        );
        output.push_str(&format!(
            "zenith_websocket_connections {}\n",
            self.websocket_connections.load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP zenith_positions_opened_total Positions opened since process start.\n\
             # TYPE zenith_positions_opened_total counter\n",
        );
        output.push_str(&format!(
            "zenith_positions_opened_total {}\n",
            self.positions_opened.load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP zenith_positions_closed_total Positions closed since process start.\n\
             # TYPE zenith_positions_closed_total counter\n",
        );
        output.push_str(&format!(
            "zenith_positions_closed_total {}\n",
            self.positions_closed.load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP zenith_premium_volume_total Premium volume traded since process start.\n\
             # TYPE zenith_premium_volume_total counter\n",
        );
        output.push_str(&format!(
            "zenith_premium_volume_total {}\n",
            *self.premium_volume.lock().unwrap()
        ));
        output.push_str(
            "# HELP zenith_collateral_locked Current collateral locked by positions.\n\
             # TYPE zenith_collateral_locked gauge\n",
        );
        output.push_str(&format!(
            "zenith_collateral_locked {}\n",
            *self.collateral_locked.lock().unwrap()
        ));
        output.push_str(
            "# HELP zenith_alert_triggers_total Alerts triggered since process start.\n\
             # TYPE zenith_alert_triggers_total counter\n",
        );
        output.push_str(&format!(
            "zenith_alert_triggers_total {}\n",
            self.alert_triggers.load(Ordering::Relaxed)
        ));
        output.push_str(
            "# HELP zenith_rate_limit_rejections_total Requests rejected by rate limits.\n\
             # TYPE zenith_rate_limit_rejections_total counter\n",
        );
        output.push_str(&format!(
            "zenith_rate_limit_rejections_total {}\n",
            self.rate_limit_rejections.load(Ordering::Relaxed)
        ));
        output
    }
}

pub struct WebSocketConnection(Arc<Metrics>);

impl Drop for WebSocketConnection {
    fn drop(&mut self) {
        self.0.websocket_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

pub struct SqlxQueryMetricsLayer;

impl SqlxQueryMetricsLayer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SqlxQueryMetricsLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Subscriber> Layer<S> for SqlxQueryMetricsLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !event.metadata().target().starts_with("sqlx::query") {
            return;
        }
        let mut visitor = QueryDurationVisitor::default();
        event.record(&mut visitor);
        let duration = visitor
            .elapsed_secs
            .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
            .or_else(|| visitor.elapsed.as_deref().and_then(parse_duration));
        global().record_database_query(duration);
        crate::telemetry::record_database_query(duration);
    }
}

#[derive(Default)]
struct QueryDurationVisitor {
    elapsed: Option<String>,
    elapsed_secs: Option<f64>,
}

impl tracing::field::Visit for QueryDurationVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "elapsed" {
            self.elapsed = Some(format!("{value:?}"));
        }
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if field.name() == "elapsed_secs" && value.is_finite() && value >= 0.0 {
            self.elapsed_secs = Some(value);
        }
    }
}

fn parse_duration(value: &str) -> Option<Duration> {
    let (number, multiplier) = if let Some(value) = value.strip_suffix("ns") {
        (value.trim(), 1e-9)
    } else if let Some(value) = value
        .strip_suffix("µs")
        .or_else(|| value.strip_suffix("us"))
    {
        (value.trim(), 1e-6)
    } else if let Some(value) = value.strip_suffix("ms") {
        (value.trim(), 1e-3)
    } else {
        (value.strip_suffix('s')?.trim(), 1.0)
    };
    let seconds = number.parse::<f64>().ok()? * multiplier;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

pub async fn handler(State(state): State<AppState>) -> impl IntoResponse {
    let healthy = state.operations.background_loop_health();
    let body = state
        .metrics
        .exposition(state.db.size(), state.db.num_idle(), healthy.into_iter());
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        body,
    )
}
