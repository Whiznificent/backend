use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};

fn is_public_market_path(path: &str) -> bool {
    matches!(
        path,
        "/api/v1/spot" | "/api/v1/price" | "/api/v1/iv" | "/api/v1/chain" | "/api/v1/stats"
    ) || path.starts_with("/api/v1/expiries/")
}

pub async fn cache_responses(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let is_get = request.method() == axum::http::Method::GET;
    let is_batch_post = request.method() == axum::http::Method::POST
        && path == "/api/v1/price/batch";
    let cacheable = is_get || is_batch_post;
    let if_none_match = request.headers().get(header::IF_NONE_MATCH).cloned();
    let mut response = next.run(request).await;
    if !cacheable {
        return response;
    }

    let cache_policy = if is_public_market_path(&path) || is_batch_post {
        "public, max-age=1, must-revalidate"
    } else {
        "private, no-store"
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static(cache_policy),
    );
    if response.status() != StatusCode::OK {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "failed to buffer response for ETag");
            parts.status = StatusCode::INTERNAL_SERVER_ERROR;
            parts.headers.remove(header::CONTENT_LENGTH);
            parts.headers.remove(header::CONTENT_ENCODING);
            return Response::from_parts(
                parts,
                Body::from(r#"{"error":"failed to prepare cached response"}"#),
            );
        }
    };
    let digest = data_encoding::HEXLOWER.encode(&Sha256::digest(&bytes));
    let etag = format!("\"{digest}\"");
    let etag_header = axum::http::HeaderValue::from_str(&etag).expect("hex ETag is a valid header");
    parts.headers.insert(header::ETAG, etag_header.clone());

    let matches = if_none_match
        .and_then(|value| value.to_str().ok().map(str::to_owned))
        .is_some_and(|header| {
            header.split(',').map(str::trim).any(|candidate| {
                candidate == "*" || (candidate.starts_with('"') && candidate == etag)
            })
        });
    if matches {
        parts.status = StatusCode::NOT_MODIFIED;
        parts.headers.remove(header::CONTENT_LENGTH);
        parts.headers.remove(header::CONTENT_ENCODING);
        Response::from_parts(parts, Body::empty())
    } else {
        Response::from_parts(parts, Body::from(bytes))
    }
}
