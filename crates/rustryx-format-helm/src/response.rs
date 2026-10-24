//! Answers built in memory: a generated or rewritten `index.yaml`.

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use sha2::{Digest, Sha256};

/// Serves `body` as `content_type`. `HEAD` carries the length and an empty body.
pub(crate) fn document(
    method: &Method,
    body: Bytes,
    content_type: &'static str,
    extra: HeaderMap,
) -> Response<Body> {
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(&body)));
    let length = u64_len(body.len());
    let payload = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(body)
    };
    let mut response = Response::new(payload);
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.extend(extra);
    response
}

fn u64_len(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// Content type of `index.yaml`.
pub(crate) const INDEX_TYPE: &str = "application/yaml";
