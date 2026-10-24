//! OCI error bodies: `{"errors":[{"code","message","detail"}]}`.
//!
//! Codes: the table of the OCI Distribution Specification 1.1 (`BLOB_UNKNOWN` ...
//! `TOOMANYREQUESTS`), plus the two that every registry client already understands and the
//! specification leaves out: `UNAVAILABLE` (a dependency is down) and `UNKNOWN` (our fault).
//! The statuses are the ones the reference registry uses, because the specification gives
//! no mapping and clients are written against those.
//!
//! Two ways an error reaches here:
//!
//! * the handler builds an [`OciError`] itself, knowing the route, so a missing blob is
//!   `BLOB_UNKNOWN` and not a generic 404;
//! * the engine or a pipeline fails with a [`FormatError`], which carries a status but no
//!   route. [`OciError::describe`] maps it by status, and a message that starts with
//!   `[CODE] ` (built by [`coded`]) names the code explicitly: that is how `classify`,
//!   which can only return a `FormatError`, says `NAME_INVALID` or `DIGEST_INVALID`.
//!
//! Client text is [`FormatError::public_message`] or text written here: nothing from SQL,
//! paths, upstream addresses or credentials reaches a body.

use std::fmt;

use axum_core::body::Body;
use http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode, header};
use rustryx_format_core::FormatError;
use serde_json::{Value, json};

use crate::headers::DISTRIBUTION_API_VERSION;

/// An error code of the registry API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OciErrorCode {
    /// Blob unknown to registry.
    BlobUnknown,
    /// Blob upload invalid.
    BlobUploadInvalid,
    /// Blob upload unknown to registry.
    BlobUploadUnknown,
    /// Provided digest did not match uploaded content.
    DigestInvalid,
    /// Manifest references a manifest or blob unknown to registry.
    ManifestBlobUnknown,
    /// Manifest invalid.
    ManifestInvalid,
    /// Manifest unknown to registry.
    ManifestUnknown,
    /// Invalid repository name.
    NameInvalid,
    /// Repository name not known to registry.
    NameUnknown,
    /// Provided length did not match content length.
    SizeInvalid,
    /// Authentication required.
    Unauthorized,
    /// Requested access to the resource is denied.
    Denied,
    /// The operation is unsupported.
    Unsupported,
    /// Too many requests.
    TooManyRequests,
    /// A dependency of the registry is unavailable.
    Unavailable,
    /// An error on the registry's side.
    Unknown,
}

impl OciErrorCode {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlobUnknown => "BLOB_UNKNOWN",
            Self::BlobUploadInvalid => "BLOB_UPLOAD_INVALID",
            Self::BlobUploadUnknown => "BLOB_UPLOAD_UNKNOWN",
            Self::DigestInvalid => "DIGEST_INVALID",
            Self::ManifestBlobUnknown => "MANIFEST_BLOB_UNKNOWN",
            Self::ManifestInvalid => "MANIFEST_INVALID",
            Self::ManifestUnknown => "MANIFEST_UNKNOWN",
            Self::NameInvalid => "NAME_INVALID",
            Self::NameUnknown => "NAME_UNKNOWN",
            Self::SizeInvalid => "SIZE_INVALID",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Denied => "DENIED",
            Self::Unsupported => "UNSUPPORTED",
            Self::TooManyRequests => "TOOMANYREQUESTS",
            Self::Unavailable => "UNAVAILABLE",
            Self::Unknown => "UNKNOWN",
        }
    }

    /// Parses a wire name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        const ALL: [OciErrorCode; 16] = [
            OciErrorCode::BlobUnknown,
            OciErrorCode::BlobUploadInvalid,
            OciErrorCode::BlobUploadUnknown,
            OciErrorCode::DigestInvalid,
            OciErrorCode::ManifestBlobUnknown,
            OciErrorCode::ManifestInvalid,
            OciErrorCode::ManifestUnknown,
            OciErrorCode::NameInvalid,
            OciErrorCode::NameUnknown,
            OciErrorCode::SizeInvalid,
            OciErrorCode::Unauthorized,
            OciErrorCode::Denied,
            OciErrorCode::Unsupported,
            OciErrorCode::TooManyRequests,
            OciErrorCode::Unavailable,
            OciErrorCode::Unknown,
        ];
        ALL.into_iter().find(|code| code.as_str() == name)
    }

    /// The code a status gets when nothing more specific is known.
    #[must_use]
    pub const fn for_status(status: StatusCode) -> Self {
        match status.as_u16() {
            401 => Self::Unauthorized,
            403 | 409 | 412 | 507 => Self::Denied,
            404 => Self::NameUnknown,
            413 => Self::SizeInvalid,
            429 => Self::TooManyRequests,
            408 | 500 => Self::Unknown,
            502..=504 => Self::Unavailable,
            400..=499 => Self::Unsupported,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for OciErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Prefixes a client-safe message with an explicit code, for the `FormatError` variants that
/// only carry text. [`OciError::describe`] strips it again.
#[must_use]
pub fn coded(code: OciErrorCode, message: &str) -> String {
    format!("[{code}] {message}")
}

/// Splits `[CODE] message`. A text without the prefix, or with an unknown code, is returned
/// whole with no code, so a stray bracket in a message is never taken for one.
fn split_coded(text: &str) -> (Option<OciErrorCode>, &str) {
    let Some(rest) = text.strip_prefix('[') else {
        return (None, text);
    };
    let Some((name, message)) = rest.split_once("] ") else {
        return (None, text);
    };
    match OciErrorCode::from_name(name) {
        Some(code) => (Some(code), message),
        None => (None, text),
    }
}

/// An error answer of the registry.
///
/// The fields live behind a box: every operation of the handler returns this type in its
/// `Err`, and a result that is a few hundred bytes wide would be copied on every `?`.
#[derive(Debug)]
pub struct OciError(Box<Parts>);

#[derive(Debug)]
struct Parts {
    status: StatusCode,
    code: OciErrorCode,
    message: String,
    detail: Option<Value>,
    headers: HeaderMap,
    /// Kept for server errors so the engine logs the real cause once.
    source: Option<FormatError>,
}

impl OciError {
    /// An error with an explicit status and code.
    #[must_use]
    pub fn new(status: StatusCode, code: OciErrorCode, message: impl Into<String>) -> Self {
        Self(Box::new(Parts {
            status,
            code,
            message: message.into(),
            detail: None,
            headers: HeaderMap::new(),
            source: None,
        }))
    }

    /// 404 `BLOB_UNKNOWN`.
    #[must_use]
    pub fn blob_unknown() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            OciErrorCode::BlobUnknown,
            "blob unknown to registry",
        )
    }

    /// 404 `MANIFEST_UNKNOWN`.
    #[must_use]
    pub fn manifest_unknown() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            OciErrorCode::ManifestUnknown,
            "manifest unknown to registry",
        )
    }

    /// 404 `NAME_UNKNOWN`.
    #[must_use]
    pub fn name_unknown() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            OciErrorCode::NameUnknown,
            "repository name not known to registry",
        )
    }

    /// 404 `BLOB_UPLOAD_UNKNOWN`.
    #[must_use]
    pub fn upload_unknown() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            OciErrorCode::BlobUploadUnknown,
            "blob upload unknown to registry",
        )
    }

    /// 400 `BLOB_UPLOAD_INVALID`.
    #[must_use]
    pub fn upload_invalid(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            OciErrorCode::BlobUploadInvalid,
            message,
        )
    }

    /// 400 `DIGEST_INVALID`.
    #[must_use]
    pub fn digest_invalid(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            OciErrorCode::DigestInvalid,
            message,
        )
    }

    /// 400 `MANIFEST_INVALID`.
    #[must_use]
    pub fn manifest_invalid(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            OciErrorCode::ManifestInvalid,
            message,
        )
    }

    /// 400 `MANIFEST_BLOB_UNKNOWN` naming the missing digest.
    #[must_use]
    pub fn manifest_blob_unknown(digest: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            OciErrorCode::ManifestBlobUnknown,
            "manifest references a manifest or blob unknown to registry",
        )
        .with_detail(json!({ "digest": digest }))
    }

    /// 400 `SIZE_INVALID`.
    #[must_use]
    pub fn size_invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, OciErrorCode::SizeInvalid, message)
    }

    /// 403 `DENIED`.
    #[must_use]
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, OciErrorCode::Denied, message)
    }

    /// An `UNSUPPORTED` error with the given status.
    #[must_use]
    pub fn unsupported(status: StatusCode, message: impl Into<String>) -> Self {
        Self::new(status, OciErrorCode::Unsupported, message)
    }

    /// 500 `UNKNOWN`. The text is for the log only: the client sees a fixed message.
    #[must_use]
    pub fn internal(cause: impl Into<String>) -> Self {
        let mut error = Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            OciErrorCode::Unknown,
            "internal error",
        );
        error.0.source = Some(FormatError::internal(cause));
        error
    }

    /// Adds the `detail` member of the error object.
    #[must_use]
    pub fn with_detail(mut self, detail: Value) -> Self {
        self.0.detail = Some(detail);
        self
    }

    /// Adds a response header (kept when the error is rendered).
    #[must_use]
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.0.headers.insert(name, value);
        self
    }

    /// The status.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.0.status
    }

    /// The code.
    #[must_use]
    pub const fn code(&self) -> OciErrorCode {
        self.0.code
    }

    /// The client-safe message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0.message
    }

    /// True for a 5xx: those go back to the engine as a `FormatError`, so they are logged
    /// once and classified (a group skips the member) like every other format's.
    #[must_use]
    pub fn is_server_error(&self) -> bool {
        self.0.status.is_server_error()
    }

    /// Re-labels a `404` as the more specific code of the route (a pipeline says only
    /// "not found").
    #[must_use]
    pub fn not_found_as(mut self, code: OciErrorCode, message: &str) -> Self {
        if self.0.status == StatusCode::NOT_FOUND {
            self.0.code = code;
            message.clone_into(&mut self.0.message);
        }
        self
    }

    /// Re-labels a `400` as the code of the route (an upload says `BLOB_UPLOAD_INVALID` where
    /// the pipeline only says "bad request").
    #[must_use]
    pub fn bad_request_as(mut self, code: OciErrorCode) -> Self {
        if self.0.status == StatusCode::BAD_REQUEST && self.0.code == OciErrorCode::Unsupported {
            self.0.code = code;
        }
        self
    }

    /// Maps an owned engine error, keeping it as the source of a server error.
    #[must_use]
    pub fn from_format_error(error: FormatError) -> Self {
        let mut described = Self::describe(&error);
        if described.is_server_error() {
            described.0.source = Some(error);
        }
        described
    }

    /// Maps an engine error by its status and variant. This is also what
    /// `FormatHandler::render_error` uses.
    #[must_use]
    pub fn describe(error: &FormatError) -> Self {
        let status = error.status();
        let (embedded, text) = match error {
            FormatError::BadRequest(m)
            | FormatError::Conflict(m)
            | FormatError::Unprocessable(m) => {
                let (code, text) = split_coded(m);
                (code, text.to_owned())
            }
            other => (None, other.public_message()),
        };
        let code = embedded.unwrap_or_else(|| match error {
            FormatError::DigestMismatch => OciErrorCode::DigestInvalid,
            FormatError::Unprocessable(_) => OciErrorCode::ManifestInvalid,
            FormatError::Policy(_) if status == StatusCode::SERVICE_UNAVAILABLE => {
                OciErrorCode::Unavailable
            }
            _ => OciErrorCode::for_status(status),
        });
        let mut described = Self::new(status, code, text);
        described.0.headers = error.headers();
        described
    }

    /// Turns a server error back into the `FormatError` the engine logs and renders. Only
    /// meaningful for [`OciError::is_server_error`]; any other error becomes an internal one.
    #[must_use]
    pub fn into_format_error(self) -> FormatError {
        self.0.source.unwrap_or_else(|| {
            FormatError::internal(format!("{}: {}", self.0.code, self.0.message))
        })
    }

    /// Renders the error. The body is `{"errors":[{"code","message","detail"}]}`.
    #[must_use]
    pub fn into_response(self) -> Response<Body> {
        let mut error = json!({ "code": self.0.code.as_str(), "message": self.0.message });
        if let Some(detail) = self.0.detail {
            error["detail"] = detail;
        }
        let body = json!({ "errors": [error] }).to_string();
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = self.0.status;
        let headers = response.headers_mut();
        headers.extend(self.0.headers);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            DISTRIBUTION_API_VERSION,
            HeaderValue::from_static("registry/2.0"),
        );
        response
    }
}

impl From<FormatError> for OciError {
    fn from(error: FormatError) -> Self {
        Self::from_format_error(error)
    }
}

impl fmt::Display for OciError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.0.status.as_u16(),
            self.0.code,
            self.0.message
        )
    }
}

#[cfg(test)]
mod tests;
