//! Composition of the OIDC pieces for one issuer, the background refresher and the status snapshot.

use std::sync::Arc;
use std::time::Duration;

use rustryx_core::ports::{Clock, SystemClock};
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::bearer::{OidcBearerAuthenticator, OidcGroupResolver};
use super::claims::ClaimsStats;
use super::config::{OidcConfig, OidcConfigError};
use super::discovery::Discovery;
use super::http::OidcHttp;
use super::jwks::JwksCache;
use super::login_flow::{FlowParts, OidcLoginFlow};
use super::validator::TokenValidator;

/// Why the provider could not be built. Contacting Keycloak is never part of it.
#[derive(Debug, thiserror::Error)]
pub enum OidcInitError {
    /// The configuration is invalid.
    #[error(transparent)]
    Config(#[from] OidcConfigError),
    /// The HTTP client could not be configured (for example an unreadable extra CA).
    #[error("oidc http client: {0}")]
    Http(String),
    /// The UI login flow could not be set up.
    #[error("oidc login flow: {0}")]
    Flow(String),
}

/// Point-in-time view for readiness flags and metrics. None of it gates `/readyz`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcStatus {
    /// A discovery document has been fetched and validated at least once.
    pub discovery_loaded: bool,
    /// The most recent contact with Keycloak (discovery, JWKS, token endpoint) worked.
    pub reachable: bool,
    /// Number of failed discovery attempts since start.
    pub discovery_failures: u64,
    /// Time of the last successful discovery.
    pub discovery_last_success: Option<OffsetDateTime>,
    /// Number of trusted signing keys in the current JWKS.
    pub jwks_keys: usize,
    /// Age of the current JWKS: the source of `oidc_jwks_age_seconds`. `None` before the first success.
    pub jwks_age: Option<Duration>,
    /// Successful JWKS fetches.
    pub jwks_refresh_ok: u64,
    /// Failed JWKS fetches.
    pub jwks_refresh_err: u64,
    /// Fetches started because of an unknown `kid`.
    pub jwks_forced_refreshes: u64,
    /// Unknown-`kid` lookups that did not fetch because the window was taken.
    pub jwks_forced_throttled: u64,
    /// JWKS entries ignored (wrong type, weak, ambiguous).
    pub jwks_keys_skipped: u64,
    /// `groups` entries dropped by the count or length cap.
    pub groups_truncated: u64,
    /// `groups` claims or entries of an unexpected shape.
    pub groups_malformed: u64,
}

/// OIDC integration for one issuer. Cheap to clone, every clone shares the same caches.
#[derive(Clone)]
pub struct OidcProvider {
    inner: Arc<Inner>,
}

struct Inner {
    config: OidcConfig,
    discovery: Arc<Discovery>,
    jwks: Arc<JwksCache>,
    validator: Arc<TokenValidator>,
    stats: Arc<ClaimsStats>,
    clock: Arc<dyn Clock>,
    flow: Option<OidcLoginFlow>,
}

impl std::fmt::Debug for OidcProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcProvider")
            .field("issuer", &self.inner.config.expected_issuer())
            .finish_non_exhaustive()
    }
}

impl OidcProvider {
    /// Builds the provider. It does not contact Keycloak: an unreachable Keycloak at start is
    /// a state (see [`Self::status`]), not an error, and the background task retries.
    pub fn new(config: OidcConfig) -> Result<Self, OidcInitError> {
        config.validate()?;
        Self::assemble(config, Arc::new(SystemClock))
    }

    /// Same as [`Self::new`] with an injected clock, for tests of time-dependent behaviour.
    pub fn with_clock(config: OidcConfig, clock: Arc<dyn Clock>) -> Result<Self, OidcInitError> {
        config.validate()?;
        Self::assemble(config, clock)
    }

    /// Skips the lower bounds of the refresh intervals so tests can observe a window
    /// that is not 30 seconds long. Never compiled into a release build.
    #[cfg(test)]
    pub(crate) fn new_for_tests(
        config: OidcConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, OidcInitError> {
        Self::assemble(config, clock)
    }

    fn assemble(config: OidcConfig, clock: Arc<dyn Clock>) -> Result<Self, OidcInitError> {
        let http = OidcHttp::new(&config).map_err(|e| OidcInitError::Http(e.to_string()))?;
        let discovery = Arc::new(Discovery::new(&config, http.clone(), Arc::clone(&clock)));
        let jwks = JwksCache::new(&config, http.clone(), Arc::clone(&discovery));
        let stats = Arc::new(ClaimsStats::default());
        let validator = Arc::new(TokenValidator::new(
            &config,
            Arc::clone(&jwks),
            Arc::clone(&stats),
        ));
        let flow = config
            .ui
            .as_ref()
            .map(|ui| {
                let parts = FlowParts {
                    discovery: Arc::clone(&discovery),
                    http,
                    validator: Arc::clone(&validator),
                };
                OidcLoginFlow::new(&config, ui, parts, Arc::clone(&clock))
                    .map_err(|e| OidcInitError::Flow(e.to_string()))
            })
            .transpose()?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                discovery,
                jwks,
                validator,
                stats,
                clock,
                flow,
            }),
        })
    }

    /// Authenticator for `Authorization: Bearer <Keycloak access token>`.
    #[must_use]
    pub fn bearer_authenticator(&self) -> OidcBearerAuthenticator {
        OidcBearerAuthenticator::new(
            Arc::clone(&self.inner.validator),
            Arc::clone(&self.inner.clock),
        )
    }

    /// Group resolver reading the `groups` claim carried in the principal.
    #[must_use]
    pub fn group_resolver(&self) -> OidcGroupResolver {
        OidcGroupResolver::new()
    }

    /// The UI login flow, `None` when the configuration has no `ui` section.
    #[must_use]
    pub fn login_flow(&self) -> Option<OidcLoginFlow> {
        self.inner.flow.clone()
    }

    /// Discovery and JWKS fetch right now, once. The error is already logged and counted.
    pub async fn refresh_now(&self) -> Result<(), String> {
        self.inner
            .discovery
            .refresh()
            .await
            .map_err(|e| e.to_string())?;
        self.inner.jwks.refresh().await.map_err(|e| e.to_string())
    }

    /// Starts the background loop: retries discovery until it works, then keeps discovery and
    /// the JWKS fresh. Stops when `shutdown` is cancelled.
    #[must_use = "keep the handle to await the task at shutdown"]
    pub fn spawn_background(&self, shutdown: CancellationToken) -> JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                let wait = this.tick().await;
                // Never sleep longer than the retry period: a fetch made on behalf of a request
                // can flip `reachable` to false at any moment, and the loop has to notice that
                // soon, or new logins would stay refused long after Keycloak is back.
                let slice = wait.min(this.inner.config.discovery_retry);
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(slice) => {}
                }
            }
        })
    }

    /// Snapshot for metrics: `oidc_jwks_age_seconds` is `jwks_age`, the discovery flag is
    /// `discovery_loaded` together with `reachable`.
    #[must_use]
    pub fn status(&self) -> OidcStatus {
        let jwks = self.inner.jwks.stats();
        OidcStatus {
            discovery_loaded: self.inner.discovery.current().is_some(),
            reachable: self.inner.discovery.reachable(),
            discovery_failures: self.inner.discovery.failures(),
            discovery_last_success: self
                .inner
                .discovery
                .last_success_unix()
                .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok()),
            jwks_keys: self.inner.jwks.key_count(),
            jwks_age: self.inner.jwks.age(),
            jwks_refresh_ok: jwks.refresh_ok,
            jwks_refresh_err: jwks.refresh_err,
            jwks_forced_refreshes: jwks.forced_started,
            jwks_forced_throttled: jwks.forced_throttled,
            jwks_keys_skipped: jwks.keys_skipped,
            groups_truncated: self.inner.stats.truncated(),
            groups_malformed: self.inner.stats.malformed(),
        }
    }

    /// One pass of the background loop; returns how long to sleep.
    async fn tick(&self) -> Duration {
        let config = &self.inner.config;
        let discovery = &self.inner.discovery;
        let jwks = &self.inner.jwks;
        // Not reachable counts as due: after an outage the first success flips `reachable`
        // back, which is what lets new interactive logins through again.
        let discovery_due = discovery.current().is_none()
            || !discovery.reachable()
            || discovery.is_due(config.discovery_refresh);
        if discovery_due && discovery.refresh().await.is_err() {
            return config.discovery_retry;
        }
        if jwks.is_due() && jwks.refresh().await.is_err() {
            return config.discovery_retry;
        }
        jwks.due_in()
            .min(discovery.due_in(config.discovery_refresh))
            .max(Duration::from_secs(1))
    }
}
