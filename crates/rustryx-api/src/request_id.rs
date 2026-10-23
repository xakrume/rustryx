//! Request id: accepted from `X-Request-Id` when well formed, generated otherwise,
//! returned on every response and available to code that builds error bodies.

use std::fmt;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::{HeaderMap, HeaderName, HeaderValue};
use uuid::Uuid;

/// Header carrying the request id in both directions.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
/// Longest accepted incoming id.
const MAX_ID_LEN: usize = 64;

tokio::task_local! {
    /// The id of the request being handled by this task.
    ///
    /// A task-local rather than a parameter because `IntoResponse::into_response`
    /// has no access to the request, yet the error envelope must carry the id.
    /// It is scoped around the whole handler future by [`request_id_layer`]; code
    /// that moves work to another task has to carry the id itself.
    static REQUEST_ID: RequestId;
}

/// A validated request id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    /// Generates a fresh id (`UUIDv7`, so ids sort by time in the logs).
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    /// Accepts a client supplied id only if it is 1 to 64 characters of
    /// `[A-Za-z0-9._-]`.
    ///
    /// The value is echoed in headers, the error body and every log line, so
    /// anything that could forge a log line, split a header or bloat the logs is
    /// refused rather than escaped.
    #[must_use]
    pub fn parse(candidate: &str) -> Option<Self> {
        let ok = !candidate.is_empty()
            && candidate.len() <= MAX_ID_LEN
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        ok.then(|| Self(candidate.to_owned()))
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn header_value(&self) -> HeaderValue {
        // Validated alphabet, so this cannot fail; the fallback keeps the type total.
        HeaderValue::from_str(&self.0).unwrap_or_else(|_| HeaderValue::from_static("invalid"))
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The id of the request running on this task, if any.
#[must_use]
pub fn current_request_id() -> Option<RequestId> {
    REQUEST_ID.try_with(Clone::clone).ok()
}

fn incoming(headers: &HeaderMap) -> Option<RequestId> {
    // Only the first header counts: several `X-Request-Id` headers are a sign of
    // a confused or hostile client, and picking one deterministically is enough.
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(RequestId::parse)
}

/// Outermost middleware: assigns the id, scopes it, and stamps the response.
pub(crate) async fn request_id_layer(mut request: Request, next: Next) -> Response {
    let id = incoming(request.headers()).unwrap_or_else(RequestId::generate);
    request.extensions_mut().insert(id.clone());
    let mut response = REQUEST_ID.scope(id.clone(), next.run(request)).await;
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, id.header_value());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_valid_and_unique() {
        let a = RequestId::generate();
        let b = RequestId::generate();
        assert_ne!(a, b);
        assert_eq!(RequestId::parse(a.as_str()), Some(a));
    }

    #[test]
    fn must_not_accept_malformed_request_ids() {
        for bad in [
            "",
            "has space",
            "new\nline",
            "carriage\rreturn",
            "quote\"",
            "semi;colon",
            "slash/path",
            "unicode-\u{e9}",
            &"a".repeat(65),
            "{\"level\":\"error\"}",
        ] {
            assert_eq!(RequestId::parse(bad), None, "{bad:?}");
        }
        assert!(RequestId::parse(&"a".repeat(64)).is_some());
        assert!(RequestId::parse("trace-1.2_3").is_some());
    }

    #[test]
    fn only_the_first_header_value_counts() {
        let mut headers = HeaderMap::new();
        headers.append(REQUEST_ID_HEADER, HeaderValue::from_static("first"));
        headers.append(REQUEST_ID_HEADER, HeaderValue::from_static("second"));
        assert_eq!(
            incoming(&headers).map(|i| i.to_string()),
            Some("first".to_owned())
        );
        headers.clear();
        headers.insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_bytes(&[0xff]).expect("opaque"),
        );
        assert_eq!(incoming(&headers), None);
    }

    #[tokio::test]
    async fn current_id_is_visible_only_inside_the_scope() {
        assert_eq!(current_request_id(), None);
        let id = RequestId::generate();
        let seen = REQUEST_ID
            .scope(id.clone(), async { current_request_id() })
            .await;
        assert_eq!(seen, Some(id));
    }
}
