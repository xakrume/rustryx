//! OIDC discovery: fetching, validating and keeping the provider endpoints.
//!
//! Failure to reach Keycloak never fails startup (README 8.2, 8.3): the state is
//! simply "no metadata yet", exposed as a flag, and the background loop retries.
//! Bearer validation and interactive login answer `ProviderUnavailable` until a
//! discovery succeeded; nothing else in the process is affected.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use openidconnect::core::CoreProviderMetadata;
use openidconnect::{AuthUrl, TokenUrl};
use rustryx_core::ports::Clock;
use url::Url;

use super::config::OidcConfig;
use super::http::{HttpError, MAX_METADATA_BYTES, OidcHttp};

/// Why a discovery attempt failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DiscoveryError {
    #[error("discovery request failed: {0}")]
    Http(#[from] HttpError),
    #[error("discovery document is invalid: {0}")]
    Invalid(String),
    #[error("issuer mismatch: configured {expected}, document says {actual}")]
    IssuerMismatch { expected: String, actual: String },
}

/// Endpoints taken from a validated discovery document.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveredProvider {
    /// Browser-facing, under the public issuer when one is configured.
    pub(crate) authorization_endpoint: AuthUrl,
    /// Back channel, under the internal issuer when a public one is configured.
    pub(crate) token_endpoint: TokenUrl,
    /// Back channel, always https.
    pub(crate) jwks_uri: Url,
}

/// The two realm URLs of a split-horizon setup.
#[derive(Debug, Clone)]
struct Horizon {
    internal: String,
    public: String,
}

/// Discovery state of one issuer, shared by the JWKS cache, the login flow and the background loop.
pub(crate) struct Discovery {
    http: OidcHttp,
    clock: Arc<dyn Clock>,
    document_url: String,
    expected_issuer: String,
    horizon: Option<Horizon>,
    current: ArcSwapOption<DiscoveredProvider>,
    /// Outcome of the most recent contact with Keycloak (discovery, JWKS or token endpoint).
    reachable: AtomicBool,
    failures: AtomicU64,
    /// Unix seconds of the last success, 0 when there was none.
    last_success: AtomicI64,
}

impl Discovery {
    pub(crate) fn new(config: &OidcConfig, http: OidcHttp, clock: Arc<dyn Clock>) -> Self {
        let internal = config.issuer_url.trim_end_matches('/').to_owned();
        let horizon = config.public_issuer_url.as_ref().map(|public| Horizon {
            internal: internal.clone(),
            public: public.trim_end_matches('/').to_owned(),
        });
        Self {
            http,
            clock,
            document_url: format!("{internal}/.well-known/openid-configuration"),
            expected_issuer: config.expected_issuer().to_owned(),
            horizon,
            current: ArcSwapOption::empty(),
            reachable: AtomicBool::new(false),
            failures: AtomicU64::new(0),
            last_success: AtomicI64::new(0),
        }
    }

    /// Endpoints of the last successful discovery.
    pub(crate) fn current(&self) -> Option<Arc<DiscoveredProvider>> {
        self.current.load_full()
    }

    /// True when the most recent contact with Keycloak worked. New interactive logins are
    /// refused with 503 while it is false (README 8.3), without touching anything else.
    pub(crate) fn reachable(&self) -> bool {
        self.reachable.load(Ordering::Relaxed)
    }

    /// Records the outcome of a call to Keycloak made by another component.
    pub(crate) fn report_contact(&self, ok: bool) {
        self.reachable.store(ok, Ordering::Relaxed);
    }

    /// True when no discovery succeeded within `period`.
    pub(crate) fn is_due(&self, period: Duration) -> bool {
        self.due_in(period).is_zero()
    }

    /// Time until the next scheduled discovery, zero when it is due.
    pub(crate) fn due_in(&self, period: Duration) -> Duration {
        let Some(last) = self.last_success_unix() else {
            return Duration::ZERO;
        };
        let elapsed = self.clock.now().unix_timestamp().saturating_sub(last);
        period.saturating_sub(Duration::from_secs(u64::try_from(elapsed).unwrap_or(0)))
    }

    pub(crate) fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }

    /// Unix seconds of the last success.
    pub(crate) fn last_success_unix(&self) -> Option<i64> {
        match self.last_success.load(Ordering::Relaxed) {
            0 => None,
            secs => Some(secs),
        }
    }

    /// One discovery attempt. A failure keeps the previous endpoints: they stay usable and
    /// a Keycloak outage must not make a node forget where the token endpoint is.
    pub(crate) async fn refresh(&self) -> Result<(), DiscoveryError> {
        match self.fetch().await {
            Ok(found) => {
                self.current.store(Some(Arc::new(found)));
                self.reachable.store(true, Ordering::Relaxed);
                self.last_success
                    .store(self.clock.now().unix_timestamp(), Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                self.reachable.store(false, Ordering::Relaxed);
                self.failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%error, url = %self.document_url, "oidc discovery failed");
                Err(error)
            }
        }
    }

    async fn fetch(&self) -> Result<DiscoveredProvider, DiscoveryError> {
        let fetched = self
            .http
            .get(&self.document_url, MAX_METADATA_BYTES)
            .await?;
        let metadata: CoreProviderMetadata = serde_json::from_slice(&fetched.body)
            .map_err(|e| DiscoveryError::Invalid(e.to_string()))?;
        // `iss` of every token is compared with this string, so a document that names a
        // different issuer would silently change what the node trusts: refuse it.
        if metadata.issuer().as_str() != self.expected_issuer {
            return Err(DiscoveryError::IssuerMismatch {
                expected: self.expected_issuer.clone(),
                actual: metadata.issuer().as_str().to_owned(),
            });
        }
        let authorization = self.browser_url(metadata.authorization_endpoint().as_str())?;
        let token_endpoint = metadata
            .token_endpoint()
            .ok_or_else(|| DiscoveryError::Invalid("token_endpoint is missing".to_owned()))?;
        let token = self.backchannel_url("token_endpoint", token_endpoint.as_str())?;
        let jwks = self.backchannel_url("jwks_uri", metadata.jwks_uri().as_str())?;
        Ok(DiscoveredProvider {
            authorization_endpoint: AuthUrl::from_url(authorization),
            token_endpoint: TokenUrl::from_url(token),
            jwks_uri: jwks,
        })
    }

    /// The authorization endpoint is opened by the browser: it must live under the public realm URL.
    fn browser_url(&self, advertised: &str) -> Result<Url, DiscoveryError> {
        let value = match &self.horizon {
            Some(h) => swap_prefix(advertised, &h.internal, &h.public),
            None => advertised.to_owned(),
        };
        Url::parse(&value)
            .map_err(|e| DiscoveryError::Invalid(format!("authorization_endpoint: {e}")))
    }

    /// Endpoints this node calls itself must be reachable from here and use TLS.
    fn backchannel_url(&self, field: &str, advertised: &str) -> Result<Url, DiscoveryError> {
        let value = match &self.horizon {
            Some(h) => swap_prefix(advertised, &h.public, &h.internal),
            None => advertised.to_owned(),
        };
        let url =
            Url::parse(&value).map_err(|e| DiscoveryError::Invalid(format!("{field}: {e}")))?;
        if url.scheme() != "https" {
            return Err(DiscoveryError::Invalid(format!("{field} must be https")));
        }
        Ok(url)
    }
}

/// Replaces the realm URL prefix `from` by `to` when `value` lives under `from`.
///
/// The character after the prefix must be a path or query boundary, so
/// `https://kc/realms/a` does not capture `https://kc/realms/ab`.
fn swap_prefix(value: &str, from: &str, to: &str) -> String {
    match value.strip_prefix(from) {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '?', '#']) => format!("{to}{rest}"),
        _ => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_prefix_respects_path_boundaries() {
        let from = "https://kc.internal/realms/a";
        let to = "https://sso.example/realms/a";
        assert_eq!(
            swap_prefix("https://kc.internal/realms/a/protocol/x", from, to),
            "https://sso.example/realms/a/protocol/x"
        );
        assert_eq!(swap_prefix(from, from, to), to);
        assert_eq!(
            swap_prefix("https://kc.internal/realms/ab/protocol/x", from, to),
            "https://kc.internal/realms/ab/protocol/x"
        );
        assert_eq!(
            swap_prefix("https://other/realms/a/x", from, to),
            "https://other/realms/a/x"
        );
    }
}
