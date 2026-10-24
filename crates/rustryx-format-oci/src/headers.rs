//! Header names of the registry protocol and small response builders.

use axum_core::body::Body;
use http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode, header};

/// `Docker-Distribution-API-Version`: `registry/2.0` on every answer, so a client can tell a
/// registry from any other server.
pub const DISTRIBUTION_API_VERSION: HeaderName =
    HeaderName::from_static("docker-distribution-api-version");
/// `Docker-Content-Digest`: the canonical digest of the manifest or blob served.
pub const DOCKER_CONTENT_DIGEST: HeaderName = HeaderName::from_static("docker-content-digest");
/// `Docker-Upload-UUID`: the id of an upload session.
pub const DOCKER_UPLOAD_UUID: HeaderName = HeaderName::from_static("docker-upload-uuid");
/// `OCI-Subject`: the subject digest of a pushed manifest that has one.
pub const OCI_SUBJECT: HeaderName = HeaderName::from_static("oci-subject");
/// `OCI-Filters-Applied`: the filters of a referrers request the registry applied.
pub const OCI_FILTERS_APPLIED: HeaderName = HeaderName::from_static("oci-filters-applied");
/// `OCI-Chunk-Min-Length`: the smallest chunk the registry wants in a `PATCH`.
pub const OCI_CHUNK_MIN_LENGTH: HeaderName = HeaderName::from_static("oci-chunk-min-length");

/// A response with a status, no body and the version header.
#[must_use]
pub fn status_only(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    stamp_version(response.headers_mut());
    response
}

/// Adds `Docker-Distribution-API-Version: registry/2.0`.
pub fn stamp_version(headers: &mut HeaderMap) {
    headers.insert(
        DISTRIBUTION_API_VERSION,
        HeaderValue::from_static("registry/2.0"),
    );
}

/// Sets a header from text; a value that is not a legal header value is dropped (every
/// caller passes digests, UUID-like tokens, numbers or paths built from validated parts).
pub fn set_text(headers: &mut HeaderMap, name: HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

/// A JSON response with the given content type.
#[must_use]
pub fn json_response(
    status: StatusCode,
    content_type: &str,
    body: &serde_json::Value,
) -> Response<Body> {
    let bytes = body.to_string();
    let mut response = Response::new(Body::from(bytes.clone()));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    set_text(headers, header::CONTENT_TYPE, content_type);
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(bytes.len() as u64),
    );
    stamp_version(headers);
    response
}
