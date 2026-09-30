use axum::body::Body;
use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::{Span as _, TraceContextExt, Tracer as _};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

static TRACER_PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();

pub fn install_provider() {
    if TRACER_PROVIDER.get().is_some() {
        return;
    }

    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "zenith-backend".to_owned());
    let resource = Resource::builder().with_service_name(service_name).build();
    let mut builder = SdkTracerProvider::builder().with_resource(resource);

    if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        let exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .expect("failed to configure OTLP trace exporter");
        builder = builder.with_batch_exporter(exporter);
    }

    let provider = builder.build();
    global::set_tracer_provider(provider.clone());
    global::set_text_map_propagator(TraceContextPropagator::new());
    TRACER_PROVIDER
        .set(provider)
        .unwrap_or_else(|_| panic!("OpenTelemetry tracer provider was already installed"));
}

pub fn shutdown() -> Result<(), opentelemetry_sdk::error::OTelSdkError> {
    if let Some(provider) = TRACER_PROVIDER.get() {
        provider.force_flush()?;
        provider.shutdown()
    } else {
        Ok(())
    }
}

pub async fn request_trace(request: Request<Body>, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(crate::logging::redact_text)
        .unwrap_or_else(|| "-".to_owned());
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "/_unmatched".to_owned());
    let parent_context = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    let span = tracing::info_span!(
        "http.request",
        request_id,
        trace_id = tracing::field::Empty,
        "http.request.method" = method,
        "http.route" = route,
        "otel.kind" = "server",
    );
    if let Err(error) = span.set_parent(parent_context) {
        tracing::warn!(error = %error, "failed to set inbound trace context");
    }

    let trace_context = span.context();
    let span_ref = trace_context.span();
    let span_context = span_ref.span_context();
    if span_context.is_valid() {
        let trace_id = span_context.trace_id().to_string();
        span.record("trace_id", trace_id.as_str());
        crate::logging::set_trace_id(request.extensions(), &trace_id);
    }

    next.run(request).instrument(span).await
}

pub fn inject_current_trace_context(headers: &mut HeaderMap) -> Result<(), String> {
    let context = tracing::Span::current().context();
    let mut injector = HeaderInjector {
        headers,
        error: None,
    };
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut injector)
    });
    injector.error.map_or(Ok(()), Err)
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}

struct HeaderInjector<'a> {
    headers: &'a mut HeaderMap,
    error: Option<String>,
}

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        let name = HeaderName::from_bytes(key.as_bytes());
        let value = HeaderValue::from_str(&value);
        match (name, value) {
            (Ok(name), Ok(value)) => {
                self.headers.insert(name, value);
            }
            (Err(error), _) => self.error = Some(error.to_string()),
            (_, Err(error)) => self.error = Some(error.to_string()),
        }
    }
}

pub fn record_database_query(duration: Option<Duration>) {
    let Some(duration) = duration else {
        return;
    };
    let end = SystemTime::now();
    let start = end.checked_sub(duration).unwrap_or(UNIX_EPOCH);
    let tracer = global::tracer("zenith-backend");
    let parent = tracing::Span::current().context();
    let mut span = tracer
        .span_builder("db.query")
        .with_kind(opentelemetry::trace::SpanKind::Client)
        .with_attributes([KeyValue::new("db.system.name", "sqlite")])
        .with_start_time(start)
        .start_with_context(&tracer, &parent);
    span.end_with_timestamp(end);
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::TraceContextExt;

    #[test]
    fn w3c_trace_context_round_trips_through_http_headers() {
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let mut incoming = HeaderMap::new();
        incoming.insert("traceparent", HeaderValue::from_static(traceparent));
        let propagator = TraceContextPropagator::new();
        let context = propagator.extract(&HeaderExtractor(&incoming));
        assert!(context.span().span_context().is_valid());
        assert_eq!(
            context.span().span_context().trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );

        let mut outgoing = HeaderMap::new();
        let mut injector = HeaderInjector {
            headers: &mut outgoing,
            error: None,
        };
        propagator.inject_context(&context, &mut injector);
        assert!(injector.error.is_none());
        assert_eq!(
            outgoing.get("traceparent").unwrap().to_str().unwrap(),
            traceparent
        );
    }

    #[test]
    fn install_provider_is_idempotent() {
        install_provider();
        install_provider();
    }
}
