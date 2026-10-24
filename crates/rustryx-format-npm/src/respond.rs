//! Building responses, and rendering errors in the registry's own shape.

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_format_core::{ErrorInfo, FormatError};
use serde_json::Value;

/// What a client is told to do after a `401`. npm clients do not act on it, curl and browsers
/// show it, and RFC 9110 requires a challenge with a `401`.
const CHALLENGE: &str = "Bearer realm=\"rustryx\", Basic realm=\"rustryx\"";

/// A JSON response with the given status.
#[must_use]
pub fn json(status: StatusCode, value: &Value) -> Response<Body> {
    bytes_with_type(status, "application/json", Bytes::from(value.to_string()))
}

/// A response with explicit bytes and content type.
#[must_use]
pub fn bytes_with_type(
    status: StatusCode,
    content_type: &'static str,
    body: Bytes,
) -> Response<Body> {
    let length = body.len() as u64;
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    response
}

/// Turns a response made by [`bytes_with_type`] into the answer to a `HEAD`: same headers, no body.
#[must_use]
pub fn for_method(method: &Method, mut response: Response<Body>) -> Response<Body> {
    if *method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    response
}

/// `{"ok":true,"id":<name>,"rev":<rev>}`, the answer of the CouchDB-shaped write routes.
#[must_use]
pub fn ok_with_id(status: StatusCode, id: &str, rev: &str) -> Response<Body> {
    json(
        status,
        &serde_json::json!({ "ok": true, "id": id, "rev": rev }),
    )
}

/// `{"ok":true}`.
#[must_use]
pub fn ok(status: StatusCode) -> Response<Body> {
    json(status, &serde_json::json!({ "ok": true }))
}

/// Copies headers into a response, replacing the ones that are already there.
pub fn extend_headers(response: &mut Response<Body>, headers: HeaderMap) {
    response.headers_mut().extend(headers);
}

/// Renders a [`FormatError`] as the registry does: `{"error": "<message>"}` (npm clients print
/// `error`, and `message` when there is one), with the stable code next to it for tools.
///
/// The message is [`FormatError::public_message`] and nothing else, so no SQL text, path or
/// upstream address can reach the body. `Retry-After`, `Allow` and `Content-Range` come from the
/// error itself; a `401` carries a `WWW-Authenticate` challenge.
#[must_use]
pub fn render_error(error: &FormatError, _info: &ErrorInfo<'_>) -> Response<Body> {
    let message = error.public_message();
    let body = serde_json::json!({
        "error": message,
        "message": message,
        "code": error.code(),
    });
    let mut response = json(error.status(), &body);
    let headers = response.headers_mut();
    headers.extend(error.headers());
    if error.status() == StatusCode::UNAUTHORIZED {
        headers.insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(CHALLENGE),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::ids::{BlobStoreId, RepositoryId};
    use rustryx_core::model::{RepoFormat, RepoType, Repository};
    use rustryx_core::names::RepoName;

    fn repository() -> Repository {
        Repository::new(
            RepositoryId::generate(),
            RepoName::parse("npm-hosted").unwrap(),
            RepoFormat::Npm,
            RepoType::Hosted,
            BlobStoreId::generate(),
            time::OffsetDateTime::UNIX_EPOCH,
        )
    }

    fn info(repository: &Repository) -> ErrorInfo<'_> {
        ErrorInfo {
            request_id: Some("r-1"),
            repository,
            anonymous: false,
        }
    }

    #[tokio::test]
    async fn errors_have_the_registry_shape_and_the_protocol_headers() {
        let repo = repository();
        let response = render_error(
            &FormatError::MethodNotAllowed { allow: "GET, HEAD" },
            &info(&repo),
        );
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "GET, HEAD");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let bytes = rustryx_format_core::stream::read_bounded(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "method not allowed");
        assert_eq!(body["code"], "method_not_allowed");
    }

    #[test]
    fn a_401_carries_a_challenge_and_a_403_does_not() {
        let repo = repository();
        let unauthorized = render_error(&FormatError::Unauthenticated, &info(&repo));
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert!(
            unauthorized
                .headers()
                .contains_key(header::WWW_AUTHENTICATE)
        );
        let forbidden = render_error(&FormatError::Forbidden, &info(&repo));
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        assert!(!forbidden.headers().contains_key(header::WWW_AUTHENTICATE));
    }

    #[test]
    fn retry_after_survives() {
        let repo = repository();
        let response = render_error(
            &FormatError::Unavailable {
                reason: "busy",
                retry_after: Some(std::time::Duration::from_secs(3)),
            },
            &info(&repo),
        );
        assert_eq!(response.headers()[header::RETRY_AFTER], "3");
    }

    #[test]
    fn head_keeps_headers_and_drops_the_body() {
        let response = for_method(
            &Method::HEAD,
            bytes_with_type(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(b"{}"),
            ),
        );
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "2");
        let kept = for_method(
            &Method::GET,
            bytes_with_type(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(b"{}"),
            ),
        );
        assert_eq!(kept.status(), StatusCode::OK);
    }
}
