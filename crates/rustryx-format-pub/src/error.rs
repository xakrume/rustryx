//! Errors in the shape the Dart client reads.
//!
//! Two things matter to `dart pub`, and both are decided here for every error the engine renders
//! for a pub repository:
//!
//! * **The body.** `Content-Type: application/vnd.pub.v2+json` and an object
//!   `{"error":{"code":..,"message":..}}`. The client prints `message`; the generic envelope of
//!   the API (`request_id` and all) is never used on these routes.
//! * **The challenge.** On `401` and `403` the response carries
//!   `WWW-Authenticate: Bearer realm="pub", message="<message>"`. The client erases its stored
//!   token on `401` and keeps it on `403`, so a refusal of a caller who presented a valid token
//!   must never be `401`. The engine already answers a known caller with 403; this module
//!   enforces it again where the response is made ([`respond`]): a `401` for a caller that is
//!   not anonymous is written as a `403`, so no code path upstream of this one can make the
//!   client throw a working token away.
//!
//! The `message` texts are fixed sentences. They contain no token, no host name and no internal
//! detail, and they stay inside the printable ASCII range without quotes or backslashes, which
//! keeps the `WWW-Authenticate` value well formed whatever the text is.

use axum_core::body::Body;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_format_core::FormatError;
use serde_json::json;

use crate::name::NameError;
use crate::version::VersionError;

/// The media type of every JSON answer.
pub const MEDIA_TYPE: &str = "application/vnd.pub.v2+json";

/// Shown with a `401`: how a caller gets a token.
const MESSAGE_UNAUTHENTICATED: &str =
    "Authentication is required. Create an API token and add it with dart pub token add.";
/// Shown with a `403`: the token is fine, the right is missing.
const MESSAGE_FORBIDDEN: &str =
    "Access denied. The credentials are valid but do not allow this operation on this repository.";

impl From<NameError> for FormatError {
    fn from(error: NameError) -> Self {
        // The text names the rule that was broken, never the input.
        Self::bad_request(error.to_string())
    }
}

impl From<VersionError> for FormatError {
    fn from(error: VersionError) -> Self {
        Self::bad_request(error.to_string())
    }
}

/// Renders an error for the pub protocol. `anonymous` is true when the caller presented no
/// credentials.
#[must_use]
pub fn render(error: &FormatError, anonymous: bool) -> Response<Body> {
    let mut status = error.status();
    if status == StatusCode::UNAUTHORIZED && !anonymous {
        // See the module documentation: a caller with a valid token is never told 401.
        status = StatusCode::FORBIDDEN;
    }
    let (code, message) = match status {
        StatusCode::UNAUTHORIZED => ("unauthenticated", MESSAGE_UNAUTHENTICATED.to_owned()),
        StatusCode::FORBIDDEN => ("forbidden", MESSAGE_FORBIDDEN.to_owned()),
        _ => (error.code(), error.public_message()),
    };
    let mut response = respond(status, code, &message);
    response.headers_mut().extend(error.headers());
    response
}

/// A pub error response with an explicit status, code and message.
#[must_use]
pub fn respond(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    let body = json!({ "error": { "code": code, "message": message } });
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(MEDIA_TYPE));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        headers.insert(header::WWW_AUTHENTICATE, challenge(message));
    }
    response
}

/// `Bearer realm="pub", message="<message>"`, with the message reduced to what a quoted string
/// can carry safely.
#[must_use]
pub fn challenge(message: &str) -> HeaderValue {
    let safe: String = message
        .chars()
        .map(|c| {
            let printable = (c.is_ascii_graphic() && c != '"' && c != '\\') || c == ' ';
            if printable { c } else { ' ' }
        })
        .collect();
    HeaderValue::from_str(&format!("Bearer realm=\"pub\", message=\"{safe}\""))
        // Unreachable: every character above is printable ASCII. The fallback keeps the
        // realm, which is the part the client needs.
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer realm=\"pub\""))
}

#[cfg(test)]
mod tests;
