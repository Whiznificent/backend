use axum::body::Body;
use axum::extract::{MatchedPath, Request};
use axum::http::Extensions;
use axum::middleware::Next;
use axum::response::Response;
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::registry::LookupSpan;

const REDACTED: &str = "[REDACTED]";

#[derive(Default)]
pub struct AccessContext {
    wallet_hash: Option<String>,
    trace_id: Option<String>,
}

pub fn set_wallet_identity(extensions: &Extensions, wallet_address: &str) {
    if let Some(context) = extensions.get::<Arc<Mutex<AccessContext>>>() {
        context.lock().unwrap().wallet_hash =
            Some(data_encoding::HEXLOWER.encode(&Sha256::digest(wallet_address.as_bytes())));
    }
}

pub fn set_trace_id(extensions: &Extensions, trace_id: &str) {
    if let Some(context) = extensions.get::<Arc<Mutex<AccessContext>>>() {
        context.lock().unwrap().trace_id = Some(trace_id.to_owned());
    }
}

pub fn access_context(extensions: &mut Extensions) -> Arc<Mutex<AccessContext>> {
    if let Some(context) = extensions.get::<Arc<Mutex<AccessContext>>>().cloned() {
        context
    } else {
        let context = Arc::new(Mutex::new(AccessContext::default()));
        extensions.insert(context.clone());
        context
    }
}

pub async fn access_log(mut request: Request<Body>, next: Next) -> Response {
    let context = access_context(request.extensions_mut());
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "/_unmatched".to_owned());
    let parent_trace_id = request
        .headers()
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_trace_id);

    let started = Instant::now();
    let response = next.run(request).await;
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    let context = context.lock().unwrap();
    let wallet_hash = context
        .wallet_hash
        .clone()
        .unwrap_or_else(|| "-".to_owned());
    let trace_id = context
        .trace_id
        .clone()
        .or(parent_trace_id)
        .unwrap_or_else(|| "-".to_owned());
    tracing::info!(
        target: "http_access",
        request_id,
        trace_id,
        method,
        route,
        status = response.status().as_u16(),
        latency_ms = started.elapsed().as_secs_f64() * 1000.0,
        wallet_hash,
        "request completed"
    );
    response
}

fn parse_trace_id(traceparent: &str) -> Option<String> {
    let mut parts = traceparent.split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let parent_id = parts.next()?;
    let flags = parts.next()?;
    if parts.next().is_some()
        || version.len() != 2
        || trace_id.len() != 32
        || parent_id.len() != 16
        || flags.len() != 2
        || ![version, trace_id, parent_id, flags]
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || trace_id.bytes().all(|byte| byte == b'0')
        || parent_id.bytes().all(|byte| byte == b'0')
    {
        return None;
    }
    Some(trace_id.to_ascii_lowercase())
}

pub struct JsonEventFormatter;

impl<S, N> FormatEvent<S, N> for JsonEventFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);
        let mut fields = visitor.0;
        let message = fields
            .remove("message")
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();

        let mut record = Map::new();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or_default();
        record.insert("timestamp".to_owned(), Value::from(timestamp));
        record.insert(
            "level".to_owned(),
            Value::from(event.metadata().level().as_str()),
        );
        record.insert("target".to_owned(), Value::from(event.metadata().target()));
        record.insert("message".to_owned(), Value::from(message));
        for name in [
            "request_id",
            "trace_id",
            "method",
            "route",
            "status",
            "latency_ms",
            "wallet_hash",
        ] {
            record.insert(name.to_owned(), fields.remove(name).unwrap_or(Value::Null));
        }
        record.insert("fields".to_owned(), Value::Object(fields));

        let json = serde_json::to_string(&Value::Object(record)).map_err(|_| std::fmt::Error)?;
        writer.write_str(&json)?;
        writer.write_char('\n')
    }
}

#[derive(Default)]
struct JsonVisitor(Map<String, Value>);

impl JsonVisitor {
    fn insert(&mut self, field: &tracing::field::Field, value: Value) {
        let value = if is_sensitive_field(field.name()) {
            Value::from(REDACTED)
        } else if matches!(field.name(), "trace_id" | "wallet_hash") {
            value
        } else {
            redact_value(value)
        };
        self.0.insert(field.name().to_owned(), value);
    }
}

impl tracing::field::Visit for JsonVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.insert(field, Value::from(format!("{value:?}")));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.insert(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.insert(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.insert(
            field,
            if value.is_finite() {
                Value::from(value)
            } else {
                Value::Null
            },
        );
    }
}

fn redact_value(value: Value) -> Value {
    match value {
        Value::String(value) => Value::from(redact_text(&value)),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_value).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_field(&key) {
                        Value::from(REDACTED)
                    } else {
                        redact_value(value)
                    };
                    (key, value)
                })
                .collect(),
        ),
        value => value,
    }
}

fn is_sensitive_field(field: &str) -> bool {
    let normalized: String = field
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "authorization",
        "bearer",
        "token",
        "nonce",
        "secret",
        "apikey",
        "email",
    ]
    .iter()
    .any(|sensitive| normalized.contains(sensitive))
        || normalized == "sig"
        || normalized.contains("signature")
}

fn redaction_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/-]+=*",
            r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b",
            r"(?i)\b(?:sk|pk|rk|api[_-]?key|gh[pousr]_|xox[baprs]?[-_]|AIza)[A-Za-z0-9_.-]{12,}\b",
            r"\b[A-Fa-f0-9]{24,}\b",
            r"\b[A-Za-z0-9+/]{80,}={0,2}\b",
            r"\b[A-Za-z0-9_-]{40,}\b",
            r#"(?i)\b(?:signature|sig|nonce|api[_-]?key(?:[_-]?secret)?|secret)\b\s*[:=]\s*["']?[^,\s"']+"#,
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("valid log redaction pattern"))
        .collect()
    })
}

pub fn redact_text(value: &str) -> String {
    redaction_patterns()
        .iter()
        .fold(value.to_owned(), |value, pattern| {
            pattern.replace_all(&value, REDACTED).into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn redacts_sensitive_fields_and_recognizable_values() {
        let text =
            redact_text("Bearer abc.def user@example.com nonce: 0123456789abcdef0123456789abcdef");
        assert!(!text.contains("abc.def"));
        assert!(!text.contains("user@example.com"));
        assert!(!text.contains("0123456789abcdef"));
        assert!(is_sensitive_field("signature"));
        assert!(is_sensitive_field("api_key_secret"));
    }

    #[test]
    fn json_events_redact_sensitive_fields_and_keep_stable_keys() {
        #[derive(Clone)]
        struct SharedWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for SharedWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = SharedWriter(output.clone());
        let subscriber = tracing_subscriber::fmt()
            .event_format(JsonEventFormatter)
            .with_writer(move || writer.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "redaction_test",
                api_key_secret = "super-secret-api-key",
                email = "alice@example.com",
                nonce = "feedfeedfeedfeedfeedfeedfeedfeed",
                trace_id = "0123456789abcdef0123456789abcdef",
                wallet_hash = "abcdef0123456789abcdef0123456789",
                "request completed"
            );
        });

        let line = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        let event: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(event["message"], "request completed");
        assert!(event["timestamp"].is_number());
        assert_eq!(event["fields"]["api_key_secret"], REDACTED);
        assert_eq!(event["fields"]["email"], REDACTED);
        assert_eq!(event["fields"]["nonce"], REDACTED);
        assert_eq!(event["trace_id"], "0123456789abcdef0123456789abcdef");
        assert_eq!(event["wallet_hash"], "abcdef0123456789abcdef0123456789");
        assert!(!line.contains("super-secret-api-key"));
        assert!(!line.contains("alice@example.com"));
        assert!(!line.contains("feedfeedfeedfeedfeedfeedfeedfeed"));
    }

    #[test]
    fn access_context_is_shared_across_request_metadata_updates() {
        let mut request = Request::builder()
            .uri("/api/v1/auth/me")
            .body(Body::empty())
            .unwrap();
        let context = access_context(request.extensions_mut());
        let wallet_address = "GTESTWALLET";
        let expected_wallet_hash = data_encoding::HEXLOWER.encode(&Sha256::digest(wallet_address));

        set_wallet_identity(request.extensions(), wallet_address);
        set_trace_id(request.extensions(), "0123456789abcdef0123456789abcdef");

        let context = context.lock().unwrap();
        assert_eq!(context.wallet_hash.as_deref(), Some(expected_wallet_hash.as_str()));
        assert_eq!(context.trace_id.as_deref(), Some("0123456789abcdef0123456789abcdef"));
    }
}
