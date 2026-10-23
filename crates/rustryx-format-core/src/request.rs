//! The request as the format layer sees it: [`RequestHead`] before classification
//! and [`ArtifactRequest`] after it.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use http::{HeaderMap, HeaderName, Method, header};
use rustryx_core::identity::{Action, Principal};
use rustryx_core::names::RepoName;
use rustryx_core::ports::auth::Coordinate;
use url::Url;

use crate::path::NormalizedPath;

/// Facts about the connection and the request that are not part of the HTTP message.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    /// Peer address after trusted-proxy handling.
    pub ip: Option<IpAddr>,
    /// `User-Agent` header, if any.
    pub user_agent: Option<String>,
    /// Request id for log and audit correlation.
    pub request_id: Option<String>,
    /// Who caused this read, for download counters. `None` lets the engine decide from the
    /// principal (a scan token is a scanner, everything else a client). Replication workers and
    /// the node's own reads set it, so that they never count as downloads.
    pub download_source: Option<rustryx_core::model::DownloadSource>,
}

/// Everything known about a request before its body is read.
///
/// The HTTP layer builds it after authentication and hands it to
/// [`crate::FormatEngine::serve`]. `path` is deliberately the raw remainder of the
/// URL path below the repository mount point (`/repository/<name>/`), still
/// percent-encoded, without leading slash: the format's `classify` decodes it
/// exactly once (see [`crate::path`]).
#[derive(Clone)]
pub struct RequestHead {
    /// HTTP method.
    pub method: Method,
    /// Raw path below the repository root, percent-encoded as received.
    pub path: String,
    /// Raw query string without the leading `?`.
    pub query: Option<String>,
    /// Request headers. The HTTP layer should mark `Authorization` as sensitive.
    pub headers: HeaderMap,
    /// Peer and correlation data.
    pub client: ClientInfo,
    /// The authenticated caller (anonymous principal when none).
    pub principal: Arc<Principal>,
    /// Externally visible base URL of the service, for example `https://repo.example.com`.
    /// Formats use it to build absolute URLs in documents (archive URLs, tarball URLs).
    pub base_url: Url,
}

impl RequestHead {
    /// Creates a head with no query, no headers and default client info.
    #[must_use]
    pub fn new(
        method: Method,
        path: impl Into<String>,
        principal: Principal,
        base_url: Url,
    ) -> Self {
        Self {
            method,
            path: path.into(),
            query: None,
            headers: HeaderMap::new(),
            client: ClientInfo::default(),
            principal: Arc::new(principal),
            base_url,
        }
    }

    /// Sets the raw query string.
    #[must_use]
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }

    /// Sets the headers.
    #[must_use]
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    /// Sets the client info.
    #[must_use]
    pub fn with_client(mut self, client: ClientInfo) -> Self {
        self.client = client;
        self
    }

    /// First value of a header as text, when it is valid visible ASCII.
    #[must_use]
    pub fn header(&self, name: &HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Declared `Content-Length`, if present and numeric.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.header(&header::CONTENT_LENGTH)
            .and_then(|v| v.trim().parse().ok())
    }

    /// First value of a query parameter, decoded.
    #[must_use]
    pub fn query_param(&self, key: &str) -> Option<String> {
        let query = self.query.as_deref()?;
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    /// Public URL of a repository: `<base>/repository/<name>`, no trailing slash.
    #[must_use]
    pub fn repository_url(&self, name: &RepoName) -> String {
        format!(
            "{}/repository/{}",
            self.base_url.as_str().trim_end_matches('/'),
            name
        )
    }

    /// True for `GET` and `HEAD`.
    #[must_use]
    pub fn is_read_method(&self) -> bool {
        self.method == Method::GET || self.method == Method::HEAD
    }
}

impl fmt::Debug for RequestHead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Credentials must not reach logs through `{:?}`; the header map prints
        // values verbatim unless they were marked sensitive, which not every
        // producer does, so redact by name here as well.
        let headers: Vec<(String, String)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let shown = if is_credential_header(name) {
                    "[REDACTED]".to_owned()
                } else {
                    String::from_utf8_lossy(value.as_bytes()).into_owned()
                };
                (name.as_str().to_owned(), shown)
            })
            .collect();
        f.debug_struct("RequestHead")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("query", &self.query.as_ref().map(|_| "[present]"))
            .field("headers", &headers)
            .field("client", &self.client)
            .field("principal", &self.principal.username)
            .finish_non_exhaustive()
    }
}

fn is_credential_header(name: &HeaderName) -> bool {
    name == header::AUTHORIZATION
        || name == header::COOKIE
        || name == header::PROXY_AUTHORIZATION
        || name == header::SET_COOKIE
}

/// What kind of document a path addresses; decides cache lifetime, timeouts and
/// group behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ContentKind {
    /// An immutable file (jar, tarball, blob). Proxy cache uses `content_max_age`,
    /// the upstream stream is limited by idle timeout only, a group returns the
    /// first member that has it and streams it without buffering.
    #[default]
    Artifact,
    /// A metadata or index document (`maven-metadata.xml`, packument, simple index,
    /// `index.yaml`). Proxy cache uses `metadata_max_age`, the whole fetch is bounded
    /// by the metadata timeout and by the document size limit, a group merges the
    /// member documents through [`crate::GroupIndexMerger`] when the format has one.
    Index,
}

/// Result of [`crate::FormatHandler::classify`]: the request after path
/// normalization, ready for authorization and routing rules.
#[derive(Debug, Clone)]
pub struct ArtifactRequest {
    /// The original request head.
    pub head: Arc<RequestHead>,
    /// Normalized path. The only path authorization and storage may use.
    pub path: NormalizedPath,
    /// Action to authorize.
    pub action: Action,
    /// Component coordinate when it can be derived without reading the body.
    pub coordinate: Option<Coordinate>,
    /// Artifact or index document.
    pub kind: ContentKind,
}

impl ArtifactRequest {
    /// Creates an artifact request without coordinate. The head is copied once into an
    /// `Arc`, so that cloning the request (group fan-out, background tasks) is cheap.
    #[must_use]
    pub fn new(head: &RequestHead, path: NormalizedPath, action: Action) -> Self {
        Self {
            head: Arc::new(head.clone()),
            path,
            action,
            coordinate: None,
            kind: ContentKind::Artifact,
        }
    }

    /// Attaches the coordinate used by content selectors.
    #[must_use]
    pub fn with_coordinate(mut self, coordinate: Coordinate) -> Self {
        self.coordinate = Some(coordinate);
        self
    }

    /// Marks the request as addressing an index document.
    #[must_use]
    pub fn index(mut self) -> Self {
        self.kind = ContentKind::Index;
        self
    }

    /// The HTTP method of the underlying request.
    #[must_use]
    pub fn method(&self) -> &Method {
        &self.head.method
    }
}

/// The default mapping from method to action: `GET`/`HEAD` read, `PUT`/`POST`/`PATCH`
/// write, `DELETE` delete. Anything else is not a content operation.
#[must_use]
pub fn action_for_method(method: &Method) -> Option<Action> {
    match *method {
        Method::GET | Method::HEAD => Some(Action::Read),
        Method::PUT | Method::POST | Method::PATCH => Some(Action::Write),
        Method::DELETE => Some(Action::Delete),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn head(method: Method) -> RequestHead {
        RequestHead::new(
            method,
            "a/b",
            Principal::anonymous(),
            Url::parse("https://repo.example.com/").unwrap(),
        )
    }

    #[test]
    fn builder_methods_set_fields() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("42"));
        let head = head(Method::PUT)
            .with_query("x=1&y=a%20b")
            .with_headers(headers)
            .with_client(ClientInfo {
                ip: Some("10.0.0.1".parse().unwrap()),
                user_agent: Some("ua".into()),
                request_id: Some("r1".into()),
                download_source: None,
            });
        assert_eq!(head.content_length(), Some(42));
        assert_eq!(head.query_param("y").as_deref(), Some("a b"));
        assert_eq!(head.query_param("z"), None);
        assert_eq!(head.header(&header::CONTENT_LENGTH), Some("42"));
        assert_eq!(head.client.request_id.as_deref(), Some("r1"));
        assert!(!head.is_read_method());
    }

    #[test]
    fn query_param_is_none_without_query() {
        assert_eq!(head(Method::GET).query_param("x"), None);
        assert!(head(Method::GET).is_read_method());
        assert!(head(Method::HEAD).is_read_method());
    }

    #[test]
    fn repository_url_has_no_double_slash() {
        let name = RepoName::parse("maven-central").unwrap();
        assert_eq!(
            head(Method::GET).repository_url(&name),
            "https://repo.example.com/repository/maven-central"
        );
    }

    #[test]
    fn must_not_leak_credentials_in_request_head_debug() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer ryx_supersecret"),
        );
        headers.insert(header::COOKIE, HeaderValue::from_static("session=abcdef"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/plain"));
        let rendered = format!(
            "{:?}",
            head(Method::GET)
                .with_headers(headers)
                .with_query("token=zzz")
        );
        assert!(!rendered.contains("ryx_supersecret"));
        assert!(!rendered.contains("abcdef"));
        assert!(!rendered.contains("zzz"));
        assert!(rendered.contains("text/plain"));
    }

    #[test]
    fn content_length_ignores_garbage() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("abc"));
        assert_eq!(
            head(Method::PUT).with_headers(headers).content_length(),
            None
        );
    }

    #[test]
    fn artifact_request_builders() {
        let head = head(Method::GET);
        let path = NormalizedPath::parse("a/b").unwrap();
        let req = ArtifactRequest::new(&head, path, Action::Read);
        assert_eq!(req.kind, ContentKind::Artifact);
        assert!(req.coordinate.is_none());
        assert_eq!(req.method(), Method::GET);
        let req = req
            .with_coordinate(Coordinate::new(None, Some("x".into()), None))
            .index();
        assert_eq!(req.kind, ContentKind::Index);
        assert_eq!(req.coordinate.unwrap().name.as_deref(), Some("x"));
    }

    #[test]
    fn method_to_action_mapping() {
        assert_eq!(action_for_method(&Method::GET), Some(Action::Read));
        assert_eq!(action_for_method(&Method::HEAD), Some(Action::Read));
        assert_eq!(action_for_method(&Method::PUT), Some(Action::Write));
        assert_eq!(action_for_method(&Method::POST), Some(Action::Write));
        assert_eq!(action_for_method(&Method::PATCH), Some(Action::Write));
        assert_eq!(action_for_method(&Method::DELETE), Some(Action::Delete));
        assert_eq!(action_for_method(&Method::OPTIONS), None);
    }
}
