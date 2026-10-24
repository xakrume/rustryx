//! Successful JSON answers.

use axum_core::body::Body;
use http::{HeaderValue, Method, Response, StatusCode, header};
use rustryx_format_core::conditional::if_none_match_hits;
use rustryx_format_core::hosted::X_RUSTRYX_CACHE;
use rustryx_format_core::{ArtifactRequest, CacheStatus};
use sha2::{Digest, Sha256};

use crate::error::MEDIA_TYPE;

/// A `200` JSON document with a strong `ETag` over the exact bytes sent. A matching
/// `If-None-Match` is answered `304`; `HEAD` gets the headers of the `GET`. The `ETag` is
/// computed here, from the document after it was rewritten for the client, and not taken from a
/// cached upstream copy: the same upstream bytes produce a different document for another
/// base URL.
#[must_use]
pub fn document(req: &ArtifactRequest, body: Vec<u8>) -> Response<Body> {
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(&body)));
    if if_none_match_hits(&req.head.headers, &etag) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        insert_etag(&mut response, &etag);
        return response;
    }
    let length = body.len() as u64;
    let body = if req.head.method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(body)
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    insert_etag(&mut response, &etag);
    response
}

/// A `200` JSON answer that is not a cacheable document (the publish steps).
#[must_use]
pub fn json(status: StatusCode, body: Vec<u8>) -> Response<Body> {
    let length = body.len() as u64;
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // A publish answer names a one time URL; nothing may keep it.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Marks how a document was produced, with the header that format-core's engine turns into the
/// `cache` label of the latency metric (and `Warning: 110` for a stale copy).
pub fn mark_cache(response: &mut Response<Body>, status: CacheStatus) {
    let headers = response.headers_mut();
    headers.insert(X_RUSTRYX_CACHE, HeaderValue::from_static(status.as_str()));
    if status == CacheStatus::Stale {
        headers.insert(
            header::WARNING,
            HeaderValue::from_static("110 - \"Response is stale\""),
        );
    }
}

fn insert_etag(response: &mut Response<Body>, etag: &str) {
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
}

#[cfg(test)]
mod tests;
