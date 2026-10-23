//! Errors of the format layer and their HTTP mapping.
//!
//! [`FormatError`] is what every pipeline step and every [`crate::FormatHandler`]
//! returns. It deliberately knows nothing about wire formats: it exposes a status
//! code, a stable machine code and a client-safe message, and each format turns
//! those into its own protocol error body in
//! [`crate::FormatHandler::render_error`]. The generic envelope
//! (`{"error":{"code","message","request_id"}}`) is only the fallback.
//!
//! Invariant: nothing in [`FormatError::public_message`] or in the generic body
//! comes from an SQL error, a filesystem path or an upstream address. Those live
//! in `Display` and in `source`, which only reach the log.

use std::time::Duration;

use axum_core::body::Body;
use http::{HeaderMap, HeaderValue, Response, StatusCode, header};
use rustryx_core::error::{BlobError, DomainError, StoreError};

use crate::path::PathError;

/// Why an upstream (proxy) call did not produce a usable answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpstreamFailureKind {
    /// DNS, TCP or TLS failure before any HTTP answer.
    Unreachable,
    /// A connect, header or idle timeout fired.
    Timeout,
    /// The upstream answered with a status the proxy cannot use (5xx after retries, 400, ...).
    BadStatus,
    /// The upstream answered 401 or 403. Never forwarded: the client cannot fix our credentials.
    Rejected,
    /// Bytes or declared checksum did not match what the upstream announced.
    Integrity,
    /// The SSRF policy refused the URL or a resolved address.
    Blocked,
    /// The circuit breaker for the upstream host is open.
    CircuitOpen,
}

impl UpstreamFailureKind {
    /// Label used in metrics and in stable error codes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Timeout => "timeout",
            Self::BadStatus => "bad_status",
            Self::Rejected => "rejected",
            Self::Integrity => "integrity",
            Self::Blocked => "blocked",
            Self::CircuitOpen => "circuit_open",
        }
    }
}

/// Failure of a format operation.
///
/// Variants carry only client-safe text where they carry text at all; internal
/// detail goes into the `detail`/`source` fields that `public_message` never reads.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FormatError {
    /// Malformed request. The message is shown to the client: keep it free of internals.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// The request carried no usable credentials where some are required (401).
    #[error("authentication required")]
    Unauthenticated,
    /// Authenticated but not allowed (403).
    #[error("forbidden")]
    Forbidden,
    /// No such artifact, or the repository is hidden from the caller (404).
    #[error("not found")]
    NotFound,
    /// The method is not supported on this path or repository type (405).
    #[error("method not allowed")]
    MethodNotAllowed {
        /// Value of the `Allow` header, for example `GET, HEAD`.
        allow: &'static str,
    },
    /// State conflict: write policy, immutable version (409). Message is client-safe.
    #[error("conflict: {0}")]
    Conflict(String),
    /// An `If-Match` or `If-Unmodified-Since` precondition failed (412).
    #[error("precondition failed")]
    PreconditionFailed,
    /// The client stopped sending the body (408).
    #[error("request body stalled")]
    RequestTimeout,
    /// The body or declared length exceeds the allowed size (413).
    #[error("payload too large (limit {limit} bytes)")]
    PayloadTooLarge {
        /// The limit that was exceeded, in bytes.
        limit: u64,
    },
    /// The requested byte range lies outside the object (416).
    #[error("range not satisfiable")]
    RangeNotSatisfiable {
        /// Size of the object, for `Content-Range: bytes */total`.
        total: u64,
    },
    /// The content is well-formed HTTP but semantically invalid, for example a bad archive (422).
    #[error("unprocessable: {0}")]
    Unprocessable(String),
    /// Too many concurrent operations; the client should retry later (429).
    #[error("too many requests")]
    TooManyRequests {
        /// Suggested delay before retrying.
        retry_after: Duration,
    },
    /// Repository or blob store quota would be exceeded (507).
    #[error("quota exceeded")]
    QuotaExceeded,
    /// A client-declared digest did not match the uploaded bytes (400).
    #[error("digest mismatch")]
    DigestMismatch,
    /// The upstream failed (502 or 504). `detail` is for logs only.
    #[error("upstream failure ({kind:?}): {detail}")]
    Upstream {
        /// What went wrong, coarsely.
        kind: UpstreamFailureKind,
        /// Log-only detail, may name the upstream host.
        detail: String,
    },
    /// A group index would exceed the merge memory cap (502).
    #[error("group index exceeds {limit} bytes")]
    IndexTooLarge {
        /// The cap that was exceeded, in bytes.
        limit: u64,
    },
    /// A dependency is down or overloaded; the client should retry (503).
    #[error("unavailable: {reason}")]
    Unavailable {
        /// Short client-safe reason.
        reason: &'static str,
        /// Suggested delay before retrying, sent as `Retry-After`.
        retry_after: Option<Duration>,
    },
    /// Metadata store failure.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Blob store failure.
    #[error(transparent)]
    Blob(#[from] BlobError),
    /// Bug or impossible state. The message is for logs only.
    #[error("internal error: {0}")]
    Internal(String),
}

impl FormatError {
    /// Shorthand for [`FormatError::BadRequest`].
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    /// Shorthand for [`FormatError::Conflict`].
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    /// Shorthand for [`FormatError::Unprocessable`].
    #[must_use]
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::Unprocessable(message.into())
    }

    /// Shorthand for [`FormatError::Internal`].
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// Builds an [`FormatError::Upstream`].
    #[must_use]
    pub fn upstream(kind: UpstreamFailureKind, detail: impl Into<String>) -> Self {
        Self::Upstream {
            kind,
            detail: detail.into(),
        }
    }

    /// HTTP status of this error.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::DigestMismatch => StatusCode::BAD_REQUEST,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed { .. } => StatusCode::METHOD_NOT_ALLOWED,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            Self::RequestTimeout => StatusCode::REQUEST_TIMEOUT,
            Self::PayloadTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RangeNotSatisfiable { .. } => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::QuotaExceeded => StatusCode::INSUFFICIENT_STORAGE,
            Self::Upstream { kind, .. } => match kind {
                UpstreamFailureKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
                _ => StatusCode::BAD_GATEWAY,
            },
            Self::IndexTooLarge { .. } => StatusCode::BAD_GATEWAY,
            Self::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Store(e) => match e {
                StoreError::NotFound(_) => StatusCode::NOT_FOUND,
                StoreError::Conflict(_) => StatusCode::CONFLICT,
                StoreError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
                StoreError::Backend { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            },
            Self::Blob(e) => match e {
                BlobError::NotFound => StatusCode::NOT_FOUND,
                BlobError::RangeNotSatisfiable => StatusCode::RANGE_NOT_SATISFIABLE,
                BlobError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
                BlobError::WriterClosed | BlobError::Backend { .. } => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            },
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable machine-readable code. Safe to expose and to put into metrics labels.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::DigestMismatch => "digest_mismatch",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed { .. } => "method_not_allowed",
            Self::Conflict(_) => "conflict",
            Self::PreconditionFailed => "precondition_failed",
            Self::RequestTimeout => "request_timeout",
            Self::PayloadTooLarge { .. } => "payload_too_large",
            Self::RangeNotSatisfiable { .. } => "range_not_satisfiable",
            Self::Unprocessable(_) => "unprocessable",
            Self::TooManyRequests { .. } => "too_many_requests",
            Self::QuotaExceeded => "quota_exceeded",
            Self::Upstream { kind, .. } => match kind {
                UpstreamFailureKind::Unreachable => "upstream_unreachable",
                UpstreamFailureKind::Timeout => "upstream_timeout",
                UpstreamFailureKind::BadStatus => "upstream_error",
                UpstreamFailureKind::Rejected => "upstream_rejected",
                UpstreamFailureKind::Integrity => "upstream_integrity",
                UpstreamFailureKind::Blocked => "upstream_blocked",
                UpstreamFailureKind::CircuitOpen => "upstream_circuit_open",
            },
            Self::IndexTooLarge { .. } => "index_too_large",
            Self::Unavailable { .. } => "unavailable",
            Self::Store(e) => match e {
                StoreError::NotFound(_) => "not_found",
                StoreError::Conflict(_) => "conflict",
                StoreError::Unavailable(_) => "unavailable",
                StoreError::Backend { .. } => "internal",
            },
            Self::Blob(e) => match e {
                BlobError::NotFound => "not_found",
                BlobError::RangeNotSatisfiable => "range_not_satisfiable",
                BlobError::Unavailable(_) => "unavailable",
                BlobError::WriterClosed | BlobError::Backend { .. } => "internal",
            },
            Self::Internal(_) => "internal",
        }
    }

    /// Message that is safe to show to the client.
    ///
    /// Only the `BadRequest`, `Conflict` and `Unprocessable` texts are passed
    /// through, because their constructors document that they must be client-safe.
    /// Everything else is a fixed string so that no SQL text, path or upstream
    /// address can reach a response body.
    #[must_use]
    pub fn public_message(&self) -> String {
        match self {
            Self::BadRequest(m) | Self::Conflict(m) | Self::Unprocessable(m) => m.clone(),
            Self::DigestMismatch => "uploaded content does not match the declared digest".into(),
            Self::Unauthenticated => "authentication required".into(),
            Self::Forbidden => "access denied".into(),
            Self::NotFound => "not found".into(),
            Self::MethodNotAllowed { .. } => "method not allowed".into(),
            Self::PreconditionFailed => "precondition failed".into(),
            Self::RequestTimeout => "request body was not received in time".into(),
            Self::PayloadTooLarge { .. } => "payload too large".into(),
            Self::RangeNotSatisfiable { .. } => "requested range not satisfiable".into(),
            Self::TooManyRequests { .. } => "too many requests, retry later".into(),
            Self::QuotaExceeded => "storage quota exceeded".into(),
            Self::Upstream { kind, .. } => match kind {
                UpstreamFailureKind::Timeout => "upstream repository timed out".into(),
                UpstreamFailureKind::Integrity => {
                    "upstream content failed integrity verification".into()
                }
                _ => "upstream repository is unavailable".into(),
            },
            Self::IndexTooLarge { .. } => "group index is too large to merge".into(),
            Self::Unavailable { reason, .. } => (*reason).to_owned(),
            Self::Store(e) => match e {
                StoreError::NotFound(_) => "not found".into(),
                StoreError::Conflict(_) => "conflict".into(),
                StoreError::Unavailable(_) => "service temporarily unavailable".into(),
                StoreError::Backend { .. } => "internal error".into(),
            },
            Self::Blob(e) => match e {
                BlobError::NotFound => "not found".into(),
                BlobError::RangeNotSatisfiable => "requested range not satisfiable".into(),
                BlobError::Unavailable(_) => "storage temporarily unavailable".into(),
                BlobError::WriterClosed | BlobError::Backend { .. } => "internal error".into(),
            },
            Self::Internal(_) => "internal error".into(),
        }
    }

    /// Suggested retry delay, if this error carries one.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::TooManyRequests { retry_after } => Some(*retry_after),
            Self::Unavailable { retry_after, .. } => *retry_after,
            Self::Store(StoreError::Unavailable(_)) | Self::Blob(BlobError::Unavailable(_)) => {
                Some(Duration::from_secs(5))
            }
            _ => None,
        }
    }

    /// Protocol headers that belong to the error itself: `Retry-After`, `Allow`,
    /// `Content-Range`. Format renderers should copy these into their response.
    #[must_use]
    pub fn headers(&self) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Some(delay) = self.retry_after() {
            // Round up: advertising 0 s would invite an immediate retry storm.
            let secs = delay.as_secs() + u64::from(delay.subsec_nanos() > 0);
            map.insert(header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
        }
        match self {
            Self::MethodNotAllowed { allow } => {
                map.insert(header::ALLOW, HeaderValue::from_static(allow));
            }
            Self::RangeNotSatisfiable { total } => {
                if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total}")) {
                    map.insert(header::CONTENT_RANGE, value);
                }
            }
            _ => {}
        }
        map
    }

    /// True for statuses that signal a fault on our side or in a dependency (5xx).
    #[must_use]
    pub fn is_server_error(&self) -> bool {
        self.status().is_server_error()
    }

    /// Renders the generic JSON envelope used when a format has no protocol error body.
    ///
    /// The body is `{"error":{"code":..,"message":..,"request_id":..}}`.
    #[must_use]
    pub fn to_generic_response(&self, request_id: Option<&str>) -> Response<Body> {
        let body = serde_json::json!({
            "error": {
                "code": self.code(),
                "message": self.public_message(),
                "request_id": request_id,
            }
        });
        let mut response = Response::new(Body::from(body.to_string()));
        *response.status_mut() = self.status();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.extend(self.headers());
        response
    }
}

impl From<PathError> for FormatError {
    fn from(_: PathError) -> Self {
        // The reason is intentionally not echoed: it would confirm to an attacker
        // which traversal variant got through an upstream filter.
        Self::BadRequest("invalid path".into())
    }
}

impl From<DomainError> for FormatError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::NotFound { .. } => Self::NotFound,
            DomainError::AlreadyExists(m) | DomainError::Conflict(m) => Self::Conflict(m),
            DomainError::Invalid(m) => Self::BadRequest(m),
            DomainError::Unauthenticated => Self::Unauthenticated,
            DomainError::Forbidden => Self::Forbidden,
            DomainError::QuotaExceeded => Self::QuotaExceeded,
            DomainError::Unavailable(_) => Self::Unavailable {
                reason: "service temporarily unavailable",
                retry_after: Some(Duration::from_secs(5)),
            },
            DomainError::Store(e) => Self::Store(e),
            DomainError::Blob(e) => Self::Blob(e),
            DomainError::Auth(e) => Self::Internal(format!("authentication backend: {e}")),
            DomainError::Internal(m) => Self::Internal(m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_variants() -> Vec<FormatError> {
        vec![
            FormatError::bad_request("bad"),
            FormatError::Unauthenticated,
            FormatError::Forbidden,
            FormatError::NotFound,
            FormatError::MethodNotAllowed { allow: "GET" },
            FormatError::conflict("c"),
            FormatError::RequestTimeout,
            FormatError::PreconditionFailed,
            FormatError::PayloadTooLarge { limit: 1 },
            FormatError::RangeNotSatisfiable { total: 9 },
            FormatError::unprocessable("u"),
            FormatError::TooManyRequests {
                retry_after: Duration::from_millis(1500),
            },
            FormatError::QuotaExceeded,
            FormatError::DigestMismatch,
            FormatError::upstream(UpstreamFailureKind::Timeout, "10.0.0.1 timed out"),
            FormatError::upstream(UpstreamFailureKind::Unreachable, "dns"),
            FormatError::upstream(UpstreamFailureKind::BadStatus, "500"),
            FormatError::upstream(UpstreamFailureKind::Rejected, "401"),
            FormatError::upstream(UpstreamFailureKind::Integrity, "sha"),
            FormatError::upstream(UpstreamFailureKind::Blocked, "ssrf"),
            FormatError::upstream(UpstreamFailureKind::CircuitOpen, "open"),
            FormatError::IndexTooLarge { limit: 5 },
            FormatError::Unavailable {
                reason: "busy",
                retry_after: None,
            },
            FormatError::Store(StoreError::NotFound("x".into())),
            FormatError::Store(StoreError::Conflict("x".into())),
            FormatError::Store(StoreError::Unavailable("pg down at 10.1.1.1".into())),
            FormatError::Blob(BlobError::NotFound),
            FormatError::Blob(BlobError::RangeNotSatisfiable),
            FormatError::Blob(BlobError::Unavailable("s3 at 10.2.2.2".into())),
            FormatError::Blob(BlobError::WriterClosed),
            FormatError::internal("/var/lib/secret/path"),
        ]
    }

    #[test]
    fn every_variant_has_status_code_message_and_generic_body() {
        for error in all_variants() {
            let status = error.status();
            assert!(
                status.is_client_error() || status.is_server_error(),
                "{error:?}"
            );
            assert_ne!(error.code(), "");
            assert_ne!(error.public_message(), "");
            let response = error.to_generic_response(Some("req-1"));
            assert_eq!(response.status(), status);
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .map(HeaderValue::as_bytes),
                Some(b"application/json".as_slice())
            );
        }
    }

    #[test]
    fn must_not_leak_internal_text_in_public_message() {
        let leaky = [
            FormatError::internal("/var/lib/secret/path"),
            FormatError::upstream(UpstreamFailureKind::Unreachable, "10.0.0.1:443 refused"),
            FormatError::Store(StoreError::Unavailable("pg at 10.1.1.1".into())),
            FormatError::Blob(BlobError::Unavailable("s3 at 10.2.2.2".into())),
            FormatError::Store(StoreError::Backend {
                context: "select secret_table",
                source: "SELECT * FROM secret_table".into(),
            }),
        ];
        for error in leaky {
            let message = error.public_message();
            for needle in ["/var/lib", "10.", "SELECT", "secret"] {
                assert!(!message.contains(needle), "{message} leaks {needle}");
            }
        }
    }

    #[test]
    fn retry_after_is_rounded_up_and_never_zero() {
        let error = FormatError::TooManyRequests {
            retry_after: Duration::from_millis(1500),
        };
        assert_eq!(
            error
                .headers()
                .get(header::RETRY_AFTER)
                .map(HeaderValue::as_bytes),
            Some(b"2".as_slice())
        );
        let tiny = FormatError::Unavailable {
            reason: "x",
            retry_after: Some(Duration::ZERO),
        };
        assert_eq!(
            tiny.headers()
                .get(header::RETRY_AFTER)
                .map(HeaderValue::as_bytes),
            Some(b"1".as_slice())
        );
    }

    #[test]
    fn protocol_headers_are_attached() {
        let allow = FormatError::MethodNotAllowed { allow: "GET, HEAD" }.headers();
        assert_eq!(
            allow.get(header::ALLOW).map(HeaderValue::as_bytes),
            Some(b"GET, HEAD".as_slice())
        );
        let range = FormatError::RangeNotSatisfiable { total: 42 }.headers();
        assert_eq!(
            range.get(header::CONTENT_RANGE).map(HeaderValue::as_bytes),
            Some(b"bytes */42".as_slice())
        );
        assert!(FormatError::NotFound.headers().is_empty());
    }

    #[test]
    fn upstream_timeout_is_504_other_upstream_failures_are_502() {
        assert_eq!(
            FormatError::upstream(UpstreamFailureKind::Timeout, "").status(),
            StatusCode::GATEWAY_TIMEOUT
        );
        for kind in [
            UpstreamFailureKind::Unreachable,
            UpstreamFailureKind::BadStatus,
            UpstreamFailureKind::Rejected,
            UpstreamFailureKind::Integrity,
            UpstreamFailureKind::Blocked,
            UpstreamFailureKind::CircuitOpen,
        ] {
            assert_eq!(
                FormatError::upstream(kind, "").status(),
                StatusCode::BAD_GATEWAY
            );
            assert_ne!(kind.as_str(), "");
        }
    }

    #[test]
    fn must_not_forward_upstream_auth_failures_as_401_or_403() {
        let error = FormatError::upstream(UpstreamFailureKind::Rejected, "upstream said 401");
        assert_ne!(error.status(), StatusCode::UNAUTHORIZED);
        assert_ne!(error.status(), StatusCode::FORBIDDEN);
        assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn domain_errors_convert_without_losing_meaning() {
        assert!(matches!(
            FormatError::from(DomainError::NotFound { entity: "x" }),
            FormatError::NotFound
        ));
        assert!(matches!(
            FormatError::from(DomainError::Forbidden),
            FormatError::Forbidden
        ));
        assert!(matches!(
            FormatError::from(DomainError::Unauthenticated),
            FormatError::Unauthenticated
        ));
        assert!(matches!(
            FormatError::from(DomainError::QuotaExceeded),
            FormatError::QuotaExceeded
        ));
        assert!(matches!(
            FormatError::from(DomainError::invalid("x")),
            FormatError::BadRequest(_)
        ));
        assert!(matches!(
            FormatError::from(DomainError::Conflict("x".into())),
            FormatError::Conflict(_)
        ));
        assert!(matches!(
            FormatError::from(DomainError::AlreadyExists("x".into())),
            FormatError::Conflict(_)
        ));
        assert!(matches!(
            FormatError::from(DomainError::Unavailable("x".into())),
            FormatError::Unavailable { .. }
        ));
        assert!(matches!(
            FormatError::from(DomainError::Internal("x".into())),
            FormatError::Internal(_)
        ));
        assert!(matches!(
            FormatError::from(DomainError::Store(StoreError::NotFound("x".into()))),
            FormatError::Store(_)
        ));
        assert!(matches!(
            FormatError::from(DomainError::Blob(BlobError::NotFound)),
            FormatError::Blob(_)
        ));
        assert!(matches!(
            FormatError::from(DomainError::Auth(rustryx_core::AuthError::Overloaded)),
            FormatError::Internal(_)
        ));
    }

    #[test]
    fn path_error_converts_to_opaque_bad_request() {
        let error = FormatError::from(PathError::Traversal);
        assert_eq!(error.public_message(), "invalid path");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }
}
