//! Small response builders shared by the routes of the harness.

use axum::body::Body;
use axum::response::Response;
use http::header::{CONTENT_TYPE, WWW_AUTHENTICATE};
use http::{HeaderValue, StatusCode};

/// The header that tells a registry client which API version answered.
pub(crate) const DISTRIBUTION_VERSION_HEADER: &str = "docker-distribution-api-version";

/// A JSON response with the given status.
pub(crate) fn json_response(status: StatusCode, body: &serde_json::Value) -> Response {
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

/// An error in the harness's own envelope (used by the `/repository` route).
pub(crate) fn envelope_error(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(
        status,
        &serde_json::json!({ "error": { "code": code, "message": message } }),
    )
}

/// An error in the OCI distribution shape (used by the `/v2` routes).
pub(crate) fn oci_error(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(
        status,
        &serde_json::json!({ "errors": [ { "code": code, "message": message } ] }),
    )
}

/// Sets `WWW-Authenticate` when the response has none, so a challenge chosen by the format
/// itself is never overwritten.
pub(crate) fn ensure_challenge(response: &mut Response, challenge: &str) {
    if response.headers().contains_key(WWW_AUTHENTICATE) {
        return;
    }
    if let Ok(value) = HeaderValue::from_str(challenge) {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
}
