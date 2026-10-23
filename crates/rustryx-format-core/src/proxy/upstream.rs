//! The upstream client contract and its reqwest implementation.
//!
//! [`UpstreamClient`] is deliberately the thinnest possible seam: one request, one
//! response head with a body stream, no redirects, no retries, no limits. All of
//! the resilience (retries, circuit breaker, concurrency cap, redirect
//! re-validation, idle timeouts) lives in [`super::executor::UpstreamExecutor`] on top of
//! it, so a test double only has to script answers.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use http::{HeaderMap, Method, StatusCode};
use url::Url;

use super::ssrf::{GuardedResolver, HostResolver, SystemResolver, UpstreamUrlPolicy, UrlRejection};
use crate::error::UpstreamFailureKind;

/// Phase in which a timeout fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutPhase {
    /// TCP or TLS connect.
    Connect,
    /// Waiting for the response head.
    Headers,
    /// No body data for the idle window.
    Idle,
    /// The total budget of a metadata fetch.
    Total,
}

/// Failure of an upstream call.
#[derive(Debug, Clone, thiserror::Error)]
pub enum UpstreamError {
    /// DNS, TCP or TLS failure. The text never contains the URL.
    #[error("connect failed: {0}")]
    Connect(String),
    /// A timeout fired.
    #[error("timeout ({0:?})")]
    Timeout(TimeoutPhase),
    /// The SSRF policy refused the URL, a redirect target or a resolved address.
    #[error("blocked by url policy: {0}")]
    Blocked(UrlRejection),
    /// The upstream sent something that is not valid HTTP, or the request could not be built.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The connection broke while the body was being read.
    #[error("body read failed: {0}")]
    Body(String),
    /// More redirects than the policy allows.
    #[error("too many redirects")]
    TooManyRedirects,
    /// The circuit breaker of this upstream host is open.
    #[error("circuit open")]
    CircuitOpen,
    /// The per-repository concurrency cap did not free up in time.
    #[error("upstream concurrency limit reached")]
    Overloaded,
}

impl UpstreamError {
    /// Coarse failure class for responses and metrics.
    #[must_use]
    pub fn kind(&self) -> UpstreamFailureKind {
        match self {
            Self::Connect(_) | Self::Body(_) | Self::Overloaded => UpstreamFailureKind::Unreachable,
            Self::Timeout(_) => UpstreamFailureKind::Timeout,
            Self::Blocked(_) => UpstreamFailureKind::Blocked,
            Self::Protocol(_) | Self::TooManyRedirects => UpstreamFailureKind::BadStatus,
            Self::CircuitOpen => UpstreamFailureKind::CircuitOpen,
        }
    }

    /// Whether another attempt could succeed (only meaningful for idempotent requests).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Connect(_) | Self::Timeout(TimeoutPhase::Connect | TimeoutPhase::Headers)
        )
    }
}

/// One outbound request. The URL has already passed the SSRF policy.
#[derive(Debug, Clone)]
pub struct UpstreamRequest {
    /// `GET` or `HEAD`.
    pub method: Method,
    /// Absolute URL.
    pub url: Url,
    /// Request headers. `Authorization` is marked sensitive so `Debug` hides it.
    pub headers: HeaderMap,
}

/// The head of an upstream answer plus its body as a stream.
pub struct UpstreamResponse {
    /// Status code.
    pub status: StatusCode,
    /// Response headers.
    pub headers: HeaderMap,
    /// Body chunks. Backpressured: the next chunk is read when polled.
    pub body: BoxStream<'static, Result<Bytes, UpstreamError>>,
}

impl std::fmt::Debug for UpstreamResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamResponse")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Sends one HTTP request upstream.
#[async_trait]
pub trait UpstreamClient: Send + Sync + 'static {
    /// Performs one request without following redirects and without retrying.
    ///
    /// Returns when the response head has arrived; the body is read from
    /// [`UpstreamResponse::body`]. Cancel safe: dropping the future (or the body
    /// stream later) aborts the request and releases the connection.
    ///
    /// Implementations must not apply a total timeout to the body (data streams are
    /// bounded by idleness only) and must not decompress: the stored bytes have to
    /// be the bytes the upstream's checksums describe.
    async fn send(&self, request: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError>;
}

/// Settings of [`ReqwestUpstreamClient`].
#[derive(Clone)]
pub struct ReqwestClientConfig {
    /// TCP and TLS connect timeout (README 8.2: 3 s).
    pub connect_timeout: Duration,
    /// `User-Agent` sent upstream.
    pub user_agent: String,
    /// DNS behind the SSRF guard. Defaults to the system resolver.
    pub resolver: Arc<dyn HostResolver>,
    /// Idle connections kept per host.
    pub pool_max_idle_per_host: usize,
}

impl Default for ReqwestClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            user_agent: concat!("rustryx/", env!("CARGO_PKG_VERSION")).to_owned(),
            resolver: Arc::new(SystemResolver),
            pool_max_idle_per_host: 32,
        }
    }
}

/// [`UpstreamClient`] on reqwest with rustls.
///
/// Notes on the choices (each one is a trap for a "simpler" client):
/// * redirects are off: the executor follows them so that each hop passes the policy;
/// * proxy environment variables are ignored: an `HTTPS_PROXY` would route traffic
///   around the pinned, validated resolution;
/// * automatic decompression is off: a `.tgz` served with a bogus
///   `Content-Encoding: gzip` would otherwise be stored unpacked and fail its checksum;
/// * the only timeout set here is connect: total timeouts would cut long downloads.
pub struct ReqwestUpstreamClient {
    client: reqwest::Client,
}

impl ReqwestUpstreamClient {
    /// Builds the client with the SSRF-guarded resolver installed.
    ///
    /// # Errors
    /// [`UpstreamError::Protocol`] when the TLS backend cannot be initialised.
    pub fn new(
        policy: Arc<UpstreamUrlPolicy>,
        config: &ReqwestClientConfig,
    ) -> Result<Self, UpstreamError> {
        let resolver = Arc::new(GuardedResolver::new(policy, Arc::clone(&config.resolver)));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .no_gzip()
            .connect_timeout(config.connect_timeout)
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .user_agent(config.user_agent.as_str())
            .dns_resolver(resolver)
            .build()
            .map_err(|e| UpstreamError::Protocol(format!("http client: {}", e.without_url())))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl UpstreamClient for ReqwestUpstreamClient {
    async fn send(&self, request: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let response = self
            .client
            .request(request.method, request.url)
            .headers(request.headers)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(map_body_error))
            .boxed();
        Ok(UpstreamResponse {
            status,
            headers,
            body,
        })
    }
}

fn find_rejection(error: &(dyn std::error::Error + 'static)) -> Option<UrlRejection> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(err) = current {
        if let Some(super::ssrf::ResolveError::Rejected(rejection)) =
            err.downcast_ref::<super::ssrf::ResolveError>()
        {
            return Some(rejection.clone());
        }
        if let Some(rejection) = err.downcast_ref::<UrlRejection>() {
            return Some(rejection.clone());
        }
        current = err.source();
    }
    None
}

fn map_reqwest_error(error: reqwest::Error) -> UpstreamError {
    // A refusal by the guarded resolver arrives wrapped in the client's own error;
    // it must stay distinguishable from a network failure, because it is a policy
    // event (audited, never retried) and not an outage.
    if let Some(rejection) = find_rejection(&error) {
        return UpstreamError::Blocked(rejection);
    }
    if error.is_timeout() {
        let phase = if error.is_connect() {
            TimeoutPhase::Connect
        } else {
            TimeoutPhase::Headers
        };
        return UpstreamError::Timeout(phase);
    }
    let is_connect = error.is_connect();
    let text = error.without_url().to_string();
    if is_connect {
        UpstreamError::Connect(text)
    } else {
        UpstreamError::Protocol(text)
    }
}

fn map_body_error(error: reqwest::Error) -> UpstreamError {
    if error.is_timeout() {
        UpstreamError::Timeout(TimeoutPhase::Idle)
    } else {
        UpstreamError::Body(error.without_url().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_kinds_and_retryability() {
        assert_eq!(
            UpstreamError::Connect("x".into()).kind(),
            UpstreamFailureKind::Unreachable
        );
        assert_eq!(
            UpstreamError::Body("x".into()).kind(),
            UpstreamFailureKind::Unreachable
        );
        assert_eq!(
            UpstreamError::Overloaded.kind(),
            UpstreamFailureKind::Unreachable
        );
        assert_eq!(
            UpstreamError::Timeout(TimeoutPhase::Idle).kind(),
            UpstreamFailureKind::Timeout
        );
        assert_eq!(
            UpstreamError::Blocked(UrlRejection::NoHost).kind(),
            UpstreamFailureKind::Blocked
        );
        assert_eq!(
            UpstreamError::Protocol("x".into()).kind(),
            UpstreamFailureKind::BadStatus
        );
        assert_eq!(
            UpstreamError::TooManyRedirects.kind(),
            UpstreamFailureKind::BadStatus
        );
        assert_eq!(
            UpstreamError::CircuitOpen.kind(),
            UpstreamFailureKind::CircuitOpen
        );

        assert!(UpstreamError::Connect("x".into()).is_retryable());
        assert!(UpstreamError::Timeout(TimeoutPhase::Connect).is_retryable());
        assert!(UpstreamError::Timeout(TimeoutPhase::Headers).is_retryable());
        assert!(!UpstreamError::Timeout(TimeoutPhase::Idle).is_retryable());
        assert!(!UpstreamError::Timeout(TimeoutPhase::Total).is_retryable());
        assert!(!UpstreamError::Blocked(UrlRejection::NoHost).is_retryable());
        assert!(!UpstreamError::CircuitOpen.is_retryable());
    }

    #[test]
    fn default_client_config_matches_the_normative_timeouts() {
        let config = ReqwestClientConfig::default();
        assert_eq!(config.connect_timeout, Duration::from_secs(3));
        assert!(config.user_agent.starts_with("rustryx/"));
    }

    #[test]
    fn client_builds_with_the_guarded_resolver() {
        assert!(
            ReqwestUpstreamClient::new(
                Arc::new(UpstreamUrlPolicy::any_public()),
                &ReqwestClientConfig::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn rejection_is_found_through_the_source_chain() {
        #[derive(Debug, thiserror::Error)]
        #[error("outer")]
        struct Outer(#[source] super::super::ssrf::ResolveError);
        let nested = Outer(super::super::ssrf::ResolveError::Rejected(
            UrlRejection::Unresolved,
        ));
        assert_eq!(find_rejection(&nested), Some(UrlRejection::Unresolved));
        let plain = std::io::Error::other("nope");
        assert_eq!(find_rejection(&plain), None);
        let lookup = super::super::ssrf::ResolveError::Lookup(std::io::Error::other("dns"));
        assert_eq!(find_rejection(&lookup), None);
    }
}
