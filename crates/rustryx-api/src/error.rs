//! The error envelope.
//!
//! Every error response of the API has the same shape:
//!
//! ```json
//! { "error": { "code": "repository_not_found", "message": "Repository not found", "request_id": "..." } }
//! ```
//!
//! `code` is a stable public string clients may switch on. `message` is written
//! here, per code, and is safe to show: it never repeats the `Display` text of a
//! backend error, so SQL, filesystem paths, upstream addresses and LDAP details
//! cannot reach a client through it. The detail goes to the log, once, when the
//! response is built (`into_response`), together with the source error chain.
//!
//! Native protocol endpoints (npm, `PyPI`, OCI, pub) answer in their own formats and
//! do not use this envelope; they can still use the `From` conversions to pick a
//! status.

use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;
use std::time::Duration;

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use rustryx_core::error::BoxedSource;
use rustryx_core::{AuthError, BlobError, DomainError, StoreError};
use serde::Serialize;

use crate::request_id::current_request_id;

/// `Retry-After` sent with 503 responses that do not know better.
pub const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(5);
/// Longest domain message passed through to a client, in characters.
const MAX_CLIENT_MESSAGE_CHARS: usize = 240;
/// Realm used when a 401 carries no explicit challenge.
const DEFAULT_REALM: &str = "rustryx";

/// A `WWW-Authenticate` challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    /// `Basic realm="...", charset="UTF-8"`.
    Basic {
        /// Realm shown by clients.
        realm: Cow<'static, str>,
    },
    /// `Bearer realm="..."` with an optional RFC 6750 error code.
    Bearer {
        /// Realm shown by clients.
        realm: Cow<'static, str>,
        /// For example `invalid_token`.
        error: Option<&'static str>,
    },
}

impl Challenge {
    /// Basic challenge for a realm.
    #[must_use]
    pub fn basic(realm: impl Into<Cow<'static, str>>) -> Self {
        Self::Basic {
            realm: realm.into(),
        }
    }

    /// Bearer challenge for a realm (the pub routes use `pub`).
    #[must_use]
    pub fn bearer(realm: impl Into<Cow<'static, str>>) -> Self {
        Self::Bearer {
            realm: realm.into(),
            error: None,
        }
    }

    /// The header value. A realm containing a quote or control character falls
    /// back to the default realm instead of producing a broken header.
    #[must_use]
    pub fn header_value(&self) -> HeaderValue {
        let safe = |realm: &str| {
            if realm
                .chars()
                .any(|c| c == '"' || c == '\\' || c.is_control())
            {
                DEFAULT_REALM.to_owned()
            } else {
                realm.to_owned()
            }
        };
        let text = match self {
            Self::Basic { realm } => {
                format!("Basic realm=\"{}\", charset=\"UTF-8\"", safe(realm))
            }
            Self::Bearer { realm, error: None } => format!("Bearer realm=\"{}\"", safe(realm)),
            Self::Bearer {
                realm,
                error: Some(error),
            } => format!("Bearer realm=\"{}\", error=\"{error}\"", safe(realm)),
        };
        HeaderValue::from_str(&text)
            .unwrap_or_else(|_| HeaderValue::from_static("Basic realm=\"rustryx\""))
    }
}

impl Default for Challenge {
    fn default() -> Self {
        Self::basic(DEFAULT_REALM)
    }
}

/// An error that renders as the envelope.
///
/// The fields live behind a box so that `Result<_, ApiError>` stays small on the
/// success path of every handler.
#[derive(Debug)]
pub struct ApiError(Box<Parts>);

#[derive(Debug)]
struct Parts {
    status: StatusCode,
    code: Cow<'static, str>,
    message: String,
    retry_after: Option<Duration>,
    challenge: Option<Challenge>,
    source: Option<BoxedSource>,
}

#[derive(Serialize)]
struct Envelope<'a> {
    error: Detail<'a>,
}

#[derive(Serialize)]
struct Detail<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<&'a str>,
}

impl ApiError {
    /// An error with an explicit status, code and client-safe message.
    #[must_use]
    pub fn new(
        status: StatusCode,
        code: impl Into<Cow<'static, str>>,
        message: impl Into<String>,
    ) -> Self {
        Self(Box::new(Parts {
            status,
            code: code.into(),
            message: message.into(),
            retry_after: None,
            challenge: None,
            source: None,
        }))
    }

    /// 400 with a client-visible reason (validation text written for clients).
    #[must_use]
    pub fn bad_request(code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            code,
            clean_message(&message.into()),
        )
    }

    /// 401, challenge added by the authentication middleware or [`with_challenge`](Self::with_challenge).
    #[must_use]
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "Authentication is required",
        )
    }

    /// 403.
    #[must_use]
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "Access denied")
    }

    /// 404 for an unmatched route.
    #[must_use]
    pub fn route_not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "route_not_found", "No such resource")
    }

    /// 405.
    #[must_use]
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Method not allowed for this resource",
        )
    }

    /// 413.
    #[must_use]
    pub fn payload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "The request body is too large",
        )
    }

    /// 408, for a client that stopped sending the body.
    #[must_use]
    pub fn request_body_timeout() -> Self {
        Self::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_body_timeout",
            "The request body was not received in time",
        )
    }

    /// 504 when producing the response took longer than the request timeout.
    #[must_use]
    pub fn request_timeout() -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
            "The request took too long to process",
        )
    }

    /// 503 with `Retry-After`: the service is overloaded.
    #[must_use]
    pub fn overloaded(retry_after: Duration) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
            "The service is overloaded, retry later",
        )
        .with_retry_after(retry_after)
    }

    /// 503 with `Retry-After`: a dependency is unavailable.
    #[must_use]
    pub fn unavailable(code: impl Into<Cow<'static, str>>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            code,
            "A required service is temporarily unavailable",
        )
        .with_retry_after(DEFAULT_RETRY_AFTER)
    }

    /// 502: an upstream answered with something unusable.
    #[must_use]
    pub fn upstream_error() -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            "The upstream repository could not serve the request",
        )
    }

    /// 504: an upstream did not answer in time.
    #[must_use]
    pub fn upstream_timeout() -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
            "The upstream repository did not answer in time",
        )
    }

    /// 500. `detail` is for the log only and never reaches the client.
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        let detail: String = detail.into();
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Internal server error",
        )
        .with_source(Box::<dyn StdError + Send + Sync>::from(detail))
    }

    /// Attaches the error that caused this one, for the log chain.
    #[must_use]
    pub fn with_source(mut self, source: BoxedSource) -> Self {
        self.0.source = Some(source);
        self
    }

    /// Sets `Retry-After`.
    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.0.retry_after = Some(retry_after);
        self
    }

    /// Sets the `WWW-Authenticate` challenge of a 401 or 403 (pub routes use
    /// `Bearer realm="pub"` on both).
    #[must_use]
    pub fn with_challenge(mut self, challenge: Challenge) -> Self {
        self.0.challenge = Some(challenge);
        self
    }

    /// Maps a failure to read a request body: a size or idle-time overrun gets
    /// its own status, anything else is a client error.
    #[must_use]
    pub fn from_body_error(error: &(dyn StdError + 'static)) -> Self {
        let mut current: Option<&(dyn StdError + 'static)> = Some(error);
        while let Some(e) = current {
            if e.is::<http_body_util::LengthLimitError>() {
                return Self::payload_too_large();
            }
            if e.is::<tower_http::timeout::TimeoutError>() {
                return Self::request_body_timeout();
            }
            current = e.source();
        }
        Self::bad_request("invalid_body", "The request body could not be read")
    }

    /// HTTP status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.0.status
    }

    /// Public error code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.0.code
    }

    /// The challenge, when one was set.
    #[must_use]
    pub fn challenge(&self) -> Option<&Challenge> {
        self.0.challenge.as_ref()
    }

    /// `Retry-After`, when set.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.0.retry_after
    }

    /// Logs once, at the boundary: server faults as errors, expected client
    /// errors as info (a 404 or 401 is not an incident).
    fn log(&self) {
        let parts = &*self.0;
        let status = parts.status.as_u16();
        let code = &*parts.code;
        let cause = parts
            .source
            .as_deref()
            .map(|s| s as &(dyn StdError + 'static));
        if parts.status.is_server_error() {
            if let Some(cause) = cause {
                tracing::error!(status, error.kind = code, error = cause, "request failed");
            } else {
                tracing::error!(status, error.kind = code, "request failed");
            }
        } else if let Some(cause) = cause {
            tracing::info!(status, error.kind = code, error = cause, "request rejected");
        } else {
            tracing::info!(status, error.kind = code, "request rejected");
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.0.status.as_u16(), self.0.code)
    }
}

impl StdError for ApiError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0
            .source
            .as_deref()
            .map(|s| s as &(dyn StdError + 'static))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        self.log();
        let Parts {
            status,
            code,
            message,
            retry_after,
            challenge,
            source: _,
        } = *self.0;
        let request_id = current_request_id();
        let envelope = Envelope {
            error: Detail {
                code: &code,
                message: &message,
                request_id: request_id
                    .as_ref()
                    .map(crate::request_id::RequestId::as_str),
            },
        };
        let body = serde_json::to_vec(&envelope).unwrap_or_else(|_| b"{}".to_vec());
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(retry) = retry_after {
            // Whole seconds, rounded up: "retry after 0" would invite an immediate retry.
            let seconds = retry.as_secs() + u64::from(retry.subsec_nanos() > 0);
            headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds.max(1)));
        }
        let challenge = match (status, challenge) {
            (_, Some(challenge)) => Some(challenge),
            // RFC 9110: a 401 must carry a challenge.
            (StatusCode::UNAUTHORIZED, None) => Some(Challenge::default()),
            _ => None,
        };
        if let Some(challenge) = challenge {
            headers.insert(header::WWW_AUTHENTICATE, challenge.header_value());
        }
        response
    }
}

/// Strips control characters and caps the length of a message that originates in
/// a service and is passed to the client. Services must still keep internals out
/// of these strings (see `DomainError` variants `Invalid`, `Conflict`,
/// `AlreadyExists`); this only bounds what a careless one can do.
fn clean_message(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let mut chars = cleaned.chars();
    let head: String = chars.by_ref().take(MAX_CLIENT_MESSAGE_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn entity_slug(entity: &str) -> String {
    entity
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        let base = match &err {
            StoreError::NotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "The resource was not found",
            ),
            StoreError::Conflict(_) => Self::new(
                StatusCode::CONFLICT,
                "conflict",
                "The request conflicts with the current state",
            ),
            StoreError::Unavailable(_) => Self::unavailable("store_unavailable"),
            StoreError::Backend { .. } => internal_from(),
        };
        base.with_source(Box::new(err))
    }
}

impl From<BlobError> for ApiError {
    fn from(err: BlobError) -> Self {
        let base = match &err {
            BlobError::NotFound => Self::new(
                StatusCode::NOT_FOUND,
                "blob_not_found",
                "The content was not found",
            ),
            BlobError::Unavailable(_) => Self::unavailable("blob_store_unavailable"),
            BlobError::RangeNotSatisfiable => Self::new(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "range_not_satisfiable",
                "The requested range cannot be satisfied",
            ),
            BlobError::WriterClosed | BlobError::Backend { .. } => internal_from(),
        };
        base.with_source(Box::new(err))
    }
}

impl From<AuthError> for ApiError {
    fn from(err: AuthError) -> Self {
        let base = match &err {
            AuthError::InvalidCredentials | AuthError::AccountDisabled => Self::new(
                // A disabled account answers exactly like a wrong password, so the
                // response cannot be used to find out which accounts exist.
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "Authentication failed",
            ),
            AuthError::InvalidToken(_) => Self::new(
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "The token is not valid",
            ),
            AuthError::TokenExpired => Self::new(
                StatusCode::UNAUTHORIZED,
                "token_expired",
                "The token has expired",
            ),
            AuthError::TokenRevoked => Self::new(
                StatusCode::UNAUTHORIZED,
                "token_revoked",
                "The token was revoked",
            ),
            AuthError::LockedOut { retry_after } => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "locked_out",
                "Too many failed attempts, try again later",
            )
            .with_retry_after(*retry_after),
            AuthError::UsernameConflict => Self::new(
                StatusCode::CONFLICT,
                "username_conflict",
                "The username is already used by another identity source",
            ),
            // Fail closed: a group lookup that failed is "no access", not "no groups".
            AuthError::GroupLookupFailed(_) => Self::forbidden(),
            AuthError::ProviderUnavailable(_) => Self::unavailable("identity_provider_unavailable"),
            AuthError::Overloaded => Self::overloaded(DEFAULT_RETRY_AFTER),
            AuthError::GuardUnavailable => Self::unavailable("login_unavailable"),
            AuthError::Backend { .. } => internal_from(),
        };
        base.with_source(Box::new(err))
    }
}

impl From<DomainError> for ApiError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::Store(inner) => Self::from(inner),
            DomainError::Blob(inner) => Self::from(inner),
            DomainError::Auth(inner) => Self::from(inner),
            DomainError::NotFound { entity } => Self::new(
                StatusCode::NOT_FOUND,
                format!("{}_not_found", entity_slug(entity)),
                format!("{} not found", capitalize(entity)),
            ),
            DomainError::AlreadyExists(ref message) => Self::new(
                StatusCode::CONFLICT,
                "already_exists",
                clean_message(message),
            )
            .with_source(Box::new(err)),
            DomainError::Conflict(ref message) => {
                Self::new(StatusCode::CONFLICT, "conflict", clean_message(message))
                    .with_source(Box::new(err))
            }
            DomainError::Invalid(ref message) => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid_input",
                clean_message(message),
            )
            .with_source(Box::new(err)),
            DomainError::Unauthenticated => Self::unauthenticated(),
            DomainError::Forbidden => Self::forbidden(),
            DomainError::QuotaExceeded => Self::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "quota_exceeded",
                "The storage quota would be exceeded",
            ),
            DomainError::Unavailable(_) => {
                Self::unavailable("dependency_unavailable").with_source(Box::new(err))
            }
            DomainError::Internal(_) => internal_from().with_source(Box::new(err)),
        }
    }
}

/// The 500 shape; the caller attaches the source.
fn internal_from() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Internal server error",
    )
}

#[cfg(test)]
mod tests;
