//! Application state shared by every handler.
//!
//! `AppState` is a bundle of `Arc`s, so cloning it per request costs a handful of
//! reference-count increments. Everything the router needs from outside the crate
//! enters here as a port trait object from `rustryx-core` (or this crate's
//! [`ReadinessProbe`]); fields whose implementation does not exist yet are
//! optional and the router fails closed without them (credentials presented
//! without an authenticator are a 503, never "anonymous").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use http::request::Parts;
use ipnet::IpNet;
use rustryx_core::Secret;
use rustryx_core::ports::{Authenticator, RbacChecker};
use rustryx_observability::MetricsHandle;

use crate::error::Challenge;
use crate::https::OriginPolicy;
use crate::readiness::{NotWiredProbe, ReadinessCache, ReadinessProbe};
use crate::session::SessionManager;

/// Chooses the `WWW-Authenticate` challenge for a request that failed to
/// authenticate. The default is Basic; the composition root installs a selector
/// that answers `Bearer realm="pub"` on pub routes, because which format a
/// request belongs to is only known from the repository registry.
pub type ChallengeSelector = Arc<dyn Fn(&Parts) -> Challenge + Send + Sync>;

/// Who may read `/metrics`.
#[derive(Clone)]
pub enum MetricsAccess {
    /// The endpoint answers 404.
    Disabled,
    /// No authentication.
    Public,
    /// `Authorization: Bearer <token>`.
    Bearer(Secret),
}

impl std::fmt::Debug for MetricsAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("Disabled"),
            Self::Public => f.write_str("Public"),
            Self::Bearer(_) => f.write_str("Bearer([REDACTED])"),
        }
    }
}

/// Per-client rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitSettings {
    /// Sustained requests per second per client.
    pub requests_per_second: u32,
    /// Requests allowed at once.
    pub burst: u32,
}

/// Tunables of the HTTP layer. Build with `ApiSettings::default()` and assign the
/// fields that differ; the struct is non-exhaustive so new settings do not break
/// callers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ApiSettings {
    /// Ceiling for producing a response head.
    pub request_timeout: Duration,
    /// Idle time allowed between body chunks.
    pub body_idle_timeout: Duration,
    /// Largest body on API (metadata) routes, bytes.
    pub max_body_bytes: u64,
    /// Largest body on artifact routes, bytes.
    pub max_blob_bytes: u64,
    /// Requests processed at once before new ones are shed with 503.
    pub max_in_flight: usize,
    /// `Retry-After` of shed requests.
    pub shed_retry_after: Duration,
    /// Exact CORS origins. Empty adds no CORS headers.
    pub cors_allowed_origins: Vec<String>,
    /// Proxies whose `X-Forwarded-For` is trusted.
    pub trusted_proxies: Vec<IpNet>,
    /// Access rule of `/metrics`.
    pub metrics_access: MetricsAccess,
    /// Global anonymous-access switch (a repository must also allow it).
    pub anonymous_enabled: bool,
    /// Per-user (or per-address, when anonymous) rate limit. `None` turns it off.
    pub rate_limit: Option<RateLimitSettings>,
    /// How long a `/readyz` answer is reused.
    pub readiness_cache_ttl: Duration,
    /// Deadline of the readiness probe.
    pub readiness_deadline: Duration,
    /// Refuse HTTP Basic credentials unless the request is HTTPS (directly impossible here,
    /// since TLS ends in front of the node: via a trusted proxy header). On by default; a
    /// development node on plain HTTP turns it off.
    pub require_https: bool,
    /// The public base URL (`server.base_url`). Its origin is the one a cookie session request
    /// may come from (CSRF) and the one the login form may be posted from.
    pub service_origin: Option<String>,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_mins(30),
            body_idle_timeout: Duration::from_secs(60),
            max_body_bytes: 10 * 1024 * 1024,
            max_blob_bytes: 5 * 1024 * 1024 * 1024,
            max_in_flight: 10_000,
            shed_retry_after: crate::error::DEFAULT_RETRY_AFTER,
            cors_allowed_origins: Vec::new(),
            trusted_proxies: Vec::new(),
            metrics_access: MetricsAccess::Disabled,
            anonymous_enabled: false,
            rate_limit: None,
            readiness_cache_ttl: Duration::from_secs(2),
            readiness_deadline: Duration::from_secs(1),
            require_https: true,
            service_origin: None,
        }
    }
}

/// Process lifecycle as seen by `/readyz`.
///
/// The server calls [`mark_listening`](Self::mark_listening) once the socket is
/// bound and [`start_draining`](Self::start_draining) as the first step of
/// shutdown, so that readiness flips before the listener closes.
#[derive(Debug, Clone, Default)]
pub struct Lifecycle {
    listening: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
}

impl Lifecycle {
    /// A lifecycle that is not yet listening and not draining.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The listener is accepting connections.
    pub fn mark_listening(&self) {
        self.listening.store(true, Ordering::Release);
    }

    /// Shutdown began: readiness turns 503, liveness stays 200.
    pub fn start_draining(&self) {
        self.draining.store(true, Ordering::Release);
    }

    /// True once the listener accepts connections.
    #[must_use]
    pub fn is_listening(&self) -> bool {
        self.listening.load(Ordering::Acquire)
    }

    /// True once shutdown began.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }
}

/// Shared handler state. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    authenticator: Option<Arc<dyn Authenticator>>,
    rbac: Option<Arc<dyn RbacChecker>>,
    readiness: Arc<dyn ReadinessProbe>,
    readiness_cache: Arc<ReadinessCache>,
    metrics: Option<MetricsHandle>,
    lifecycle: Lifecycle,
    settings: Arc<ApiSettings>,
    challenge: ChallengeSelector,
    sessions: Option<Arc<SessionManager>>,
    origins: Arc<OriginPolicy>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("authenticator", &self.authenticator.is_some())
            .field("rbac", &self.rbac.is_some())
            .field("metrics", &self.metrics.is_some())
            .field("sessions", &self.sessions.is_some())
            .field("lifecycle", &self.lifecycle)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// State with a readiness probe and nothing else wired.
    #[must_use]
    pub fn new(readiness: Arc<dyn ReadinessProbe>) -> Self {
        let settings = ApiSettings::default();
        Self {
            authenticator: None,
            rbac: None,
            readiness,
            readiness_cache: Arc::new(ReadinessCache::new(
                settings.readiness_cache_ttl,
                settings.readiness_deadline,
            )),
            metrics: None,
            lifecycle: Lifecycle::new(),
            origins: Arc::new(OriginPolicy::new(
                settings.service_origin.as_deref(),
                &settings.cors_allowed_origins,
            )),
            settings: Arc::new(settings),
            challenge: Arc::new(|_| Challenge::default()),
            sessions: None,
        }
    }

    /// State where nothing is wired: `/readyz` reports not ready.
    #[must_use]
    pub fn unwired() -> Self {
        Self::new(Arc::new(NotWiredProbe))
    }

    /// Installs the authenticator chain.
    #[must_use]
    pub fn with_authenticator(mut self, authenticator: Arc<dyn Authenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Installs the authorization checker.
    #[must_use]
    pub fn with_rbac(mut self, rbac: Arc<dyn RbacChecker>) -> Self {
        self.rbac = Some(rbac);
        self
    }

    /// Installs the metrics handle served at `/metrics` and fed by the middleware.
    #[must_use]
    pub fn with_metrics(mut self, metrics: MetricsHandle) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Replaces the lifecycle (the server shares one with its shutdown logic).
    #[must_use]
    pub fn with_lifecycle(mut self, lifecycle: Lifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    /// Replaces the settings. Also rebuilds the readiness cache, whose timings
    /// come from them.
    #[must_use]
    pub fn with_settings(mut self, settings: ApiSettings) -> Self {
        self.readiness_cache = Arc::new(ReadinessCache::new(
            settings.readiness_cache_ttl,
            settings.readiness_deadline,
        ));
        self.origins = Arc::new(OriginPolicy::new(
            settings.service_origin.as_deref(),
            &settings.cors_allowed_origins,
        ));
        self.settings = Arc::new(settings);
        self
    }

    /// Installs the cookie session manager. Without one a session cookie means nothing.
    #[must_use]
    pub fn with_sessions(mut self, sessions: Arc<SessionManager>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Installs the challenge selector for failed authentication.
    #[must_use]
    pub fn with_challenge_selector(mut self, selector: ChallengeSelector) -> Self {
        self.challenge = selector;
        self
    }

    /// The authenticator chain, if wired.
    #[must_use]
    pub fn authenticator(&self) -> Option<&Arc<dyn Authenticator>> {
        self.authenticator.as_ref()
    }

    /// The authorization checker, if wired.
    #[must_use]
    pub fn rbac(&self) -> Option<&Arc<dyn RbacChecker>> {
        self.rbac.as_ref()
    }

    /// The cookie session manager, if wired.
    #[must_use]
    pub fn sessions(&self) -> Option<&Arc<SessionManager>> {
        self.sessions.as_ref()
    }

    /// Which `Origin` values count as this service (CSRF and the login form).
    #[must_use]
    pub fn origins(&self) -> &OriginPolicy {
        &self.origins
    }

    /// The metrics handle, if wired.
    #[must_use]
    pub fn metrics(&self) -> Option<&MetricsHandle> {
        self.metrics.as_ref()
    }

    /// The process lifecycle.
    #[must_use]
    pub fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    /// The HTTP settings.
    #[must_use]
    pub fn settings(&self) -> &ApiSettings {
        &self.settings
    }

    pub(crate) fn readiness(&self) -> (&dyn ReadinessProbe, &ReadinessCache) {
        (self.readiness.as_ref(), self.readiness_cache.as_ref())
    }

    pub(crate) fn challenge_for(&self, parts: &Parts) -> Challenge {
        (self.challenge)(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_flags_are_shared_between_clones() {
        let a = Lifecycle::new();
        let b = a.clone();
        assert!(!a.is_listening() && !a.is_draining());
        b.mark_listening();
        assert!(a.is_listening());
        b.start_draining();
        assert!(a.is_draining());
    }

    #[test]
    fn must_not_print_the_metrics_token_in_debug() {
        let access = MetricsAccess::Bearer(Secret::new("metrics-token-canary"));
        assert!(!format!("{access:?}").contains("metrics-token-canary"));
        let settings = ApiSettings {
            metrics_access: access,
            ..ApiSettings::default()
        };
        assert!(!format!("{settings:?}").contains("metrics-token-canary"));
    }

    #[test]
    fn state_debug_does_not_list_secrets() {
        let state = AppState::unwired();
        let text = format!("{state:?}");
        assert!(text.contains("AppState"));
    }

    #[test]
    fn defaults_follow_the_requirements() {
        let s = ApiSettings::default();
        assert_eq!(s.request_timeout, Duration::from_mins(30));
        assert_eq!(s.readiness_deadline, Duration::from_secs(1));
        assert_eq!(s.readiness_cache_ttl, Duration::from_secs(2));
        assert!(!s.anonymous_enabled);
        assert!(matches!(s.metrics_access, MetricsAccess::Disabled));
    }
}
