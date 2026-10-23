//! Authorization code flow with PKCE (S256) for the confidential UI client.
//!
//! Only pure logic lives here: the HTTP handlers of `rustryx-api` call [`OidcLoginFlow::start`]
//! on `/login` and [`OidcLoginFlow::complete`] on the callback. Password grant, implicit
//! flow and public clients do not exist in this module on purpose: the code cannot be
//! configured into them.
//!
//! What binds the callback to the browser that started the login: `state`, `nonce` and the PKCE
//! verifier are sealed into one cookie (AES-256-GCM), so any node can finish a login that
//! another node began. A login CSRF (attacker's code delivered to the victim) fails the
//! `state` comparison; a stolen code is useless without the verifier in the sealed cookie.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreJsonWebKeySet};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet, IssuerUrl,
    Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    RequestTokenError, Scope, TokenResponse,
};
use rustryx_core::AuthError;
use rustryx_core::ports::Clock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

use super::claims::OidcIdentity;
use super::config::{OidcConfig, SCOPE_OFFLINE_ACCESS, UiClientConfig};
use super::cookie::{OpenError, SealError, SealedCookie};
use super::discovery::{DiscoveredProvider, Discovery};
use super::http::OidcHttp;
use super::validator::TokenValidator;

/// Replay memory capacity. Eviction only weakens the per-node replay check; the one-time
/// authorization code at Keycloak remains the authoritative single-use guard.
const CONSUMED_STATE_CAPACITY: usize = 100_000;
/// Margin added to the cookie lifetime when remembering a used `state`.
const CONSUMED_STATE_MARGIN: Duration = Duration::from_secs(60);
const MAX_CODE_BYTES: usize = 4096;
const MAX_STATE_BYTES: usize = 512;

type FlowClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
    EndpointNotSet,
>;

/// What the browser carries between the redirect and the callback. Sealed, never logged.
#[derive(Serialize, Deserialize)]
struct FlowState {
    state: String,
    nonce: String,
    verifier: String,
}

/// Result of [`OidcLoginFlow::start`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginStart {
    /// Where to redirect the browser (`302 Location`).
    pub authorization_url: Url,
    /// Full `Set-Cookie` header value for the flow cookie, with `HttpOnly; Secure; SameSite=Lax`.
    pub set_cookie: String,
}

/// Interactive OIDC login of the UI.
#[derive(Clone)]
pub struct OidcLoginFlow {
    inner: Arc<FlowInner>,
}

struct FlowInner {
    discovery: Arc<Discovery>,
    http: OidcHttp,
    validator: Arc<TokenValidator>,
    cookie: SealedCookie,
    client_id: String,
    client_secret: String,
    issuer: IssuerUrl,
    redirect: RedirectUrl,
    scopes: Vec<Scope>,
    consumed: Mutex<ConsumedStates>,
}

impl std::fmt::Debug for OidcLoginFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcLoginFlow")
            .field("client_id", &self.inner.client_id)
            .field("cookie", &self.inner.cookie)
            .finish_non_exhaustive()
    }
}

/// Why the flow could not be built.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FlowInitError {
    #[error("invalid url: {0}")]
    Redirect(String),
    #[error("cannot build flow cookie: {0}")]
    Cookie(#[from] SealError),
}

impl OidcLoginFlow {
    pub(crate) fn new(
        config: &OidcConfig,
        ui: &UiClientConfig,
        parts: FlowParts,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, FlowInitError> {
        let redirect_url =
            Url::parse(&ui.redirect_url).map_err(|e| FlowInitError::Redirect(e.to_string()))?;
        let redirect = RedirectUrl::new(ui.redirect_url.clone())
            .map_err(|e| FlowInitError::Redirect(e.to_string()))?;
        let issuer = IssuerUrl::new(config.expected_issuer().to_owned())
            .map_err(|e| FlowInitError::Redirect(format!("issuer: {e}")))?;
        // The cookie only needs to reach the callback: scoping it keeps it off every other request.
        let cookie = SealedCookie::new(&ui.cookie_key, ui.cookie_name.clone(), ui.flow_ttl, clock)?
            .with_path(redirect_url.path());
        // `openid` is added by the client itself; `offline_access` was refused by validation and
        // is filtered again here so that no code path can ever request it.
        let scopes = ui
            .scopes
            .iter()
            .filter(|scope| !matches!(scope.as_str(), "openid" | SCOPE_OFFLINE_ACCESS))
            .map(|scope| Scope::new(scope.clone()))
            .collect();
        let consumed = Mutex::new(ConsumedStates::new(
            ui.flow_ttl + CONSUMED_STATE_MARGIN,
            CONSUMED_STATE_CAPACITY,
        ));
        Ok(Self {
            inner: Arc::new(FlowInner {
                discovery: parts.discovery,
                http: parts.http,
                validator: parts.validator,
                cookie,
                client_id: ui.client_id.clone(),
                client_secret: ui.client_secret.expose().to_owned(),
                issuer,
                redirect,
                scopes,
                consumed,
            }),
        })
    }

    /// Begins a login: the authorization URL (PKCE S256, `state`, `nonce`) and the sealed cookie.
    ///
    /// Answers `ProviderUnavailable` (503) while Keycloak cannot be reached, so users get a
    /// clear message instead of a redirect to a dead host. Nothing else is affected.
    pub fn start(&self) -> Result<LoginStart, AuthError> {
        let provider = self.provider()?;
        if !self.inner.discovery.reachable() {
            return Err(unavailable("keycloak is not reachable"));
        }
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let client = self.client(&provider);
        let mut request = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge);
        for scope in &self.inner.scopes {
            request = request.add_scope(scope.clone());
        }
        let (authorization_url, state, nonce) = request.url();
        let sealed = self
            .inner
            .cookie
            .seal(&FlowState {
                state: state.secret().clone(),
                nonce: nonce.secret().clone(),
                verifier: verifier.secret().clone(),
            })
            .map_err(|error| backend("seal flow cookie", error))?;
        Ok(LoginStart {
            authorization_url,
            set_cookie: self.inner.cookie.set_cookie_header(&sealed),
        })
    }

    /// Finishes a login: checks the cookie and `state`, exchanges the code (with the PKCE
    /// verifier and the client secret), validates the ID token and returns the identity.
    ///
    /// Errors map to HTTP as follows: `InvalidToken` (cookie, state, ID token) is a 400,
    /// `InvalidCredentials` (Keycloak refused the code) a 401, `ProviderUnavailable` a 503.
    pub async fn complete(
        &self,
        code: &str,
        state: &str,
        cookie_value: &str,
    ) -> Result<OidcIdentity, AuthError> {
        if code.is_empty() || code.len() > MAX_CODE_BYTES || state.len() > MAX_STATE_BYTES {
            return Err(AuthError::InvalidToken("malformed callback"));
        }
        let flow: FlowState = self.inner.cookie.open(cookie_value).map_err(|error| {
            tracing::debug!(%error, "oidc flow cookie rejected");
            AuthError::InvalidToken(match error {
                OpenError::Malformed => "flow cookie malformed",
                OpenError::Authentication => "flow cookie invalid",
                OpenError::Expired => "flow cookie expired",
            })
        })?;
        // Constant time: `state` is a secret of this browser session.
        if state.is_empty() || !bool::from(state.as_bytes().ct_eq(flow.state.as_bytes())) {
            return Err(AuthError::InvalidToken("state mismatch"));
        }
        // Marked used before the network call so two concurrent callbacks with one cookie cannot
        // both proceed. A transient failure below costs the user one restart of the login.
        if !self.consume_state(&flow.state) {
            return Err(AuthError::InvalidToken("state already used"));
        }
        let provider = self.provider()?;
        let response = self
            .client(&provider)
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(flow.verifier))
            .request_async(&self.inner.http)
            .await
            .map_err(|error| self.token_error(&error))?;
        self.inner.discovery.report_contact(true);
        if response
            .scopes()
            .is_some_and(|scopes| scopes.iter().any(|s| s.as_str() == SCOPE_OFFLINE_ACCESS))
        {
            return Err(AuthError::InvalidToken("offline tokens are not accepted"));
        }
        let id_token = response
            .id_token()
            .ok_or(AuthError::InvalidToken("token response has no id token"))?
            .to_string();
        let verified = self
            .inner
            .validator
            .validate_id_token(&id_token, &self.inner.client_id, &flow.nonce)
            .await?;
        Ok(verified.identity)
    }

    /// `Set-Cookie` header value that removes the flow cookie. Send it with the callback response.
    #[must_use]
    pub fn clear_cookie(&self) -> String {
        self.inner.cookie.clear_cookie_header()
    }

    /// Name of the flow cookie, for the handler that reads it from the request.
    #[must_use]
    pub fn cookie_name(&self) -> &str {
        self.inner.cookie.name()
    }

    fn provider(&self) -> Result<Arc<DiscoveredProvider>, AuthError> {
        self.inner
            .discovery
            .current()
            .ok_or_else(|| unavailable("oidc discovery has not succeeded yet"))
    }

    fn client(&self, provider: &DiscoveredProvider) -> FlowClient {
        // Token validation is done by `TokenValidator`, not by the client's own verifier, so the
        // key set handed to the client stays empty.
        CoreClient::new(
            ClientId::new(self.inner.client_id.clone()),
            self.inner.issuer.clone(),
            CoreJsonWebKeySet::new(Vec::new()),
        )
        .set_client_secret(ClientSecret::new(self.inner.client_secret.clone()))
        .set_auth_uri(provider.authorization_endpoint.clone())
        .set_token_uri(provider.token_endpoint.clone())
        .set_redirect_uri(self.inner.redirect.clone())
    }

    /// True the first time a `state` is seen. The lock is held for a map insert only, never
    /// across an await point.
    fn consume_state(&self, state: &str) -> bool {
        let key: [u8; 32] = Sha256::digest(state.as_bytes()).into();
        self.inner
            .consumed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first_use(key, Instant::now())
    }

    fn token_error<E>(&self, error: &RequestTokenError<super::http::HttpError, E>) -> AuthError
    where
        E: openidconnect::ErrorResponse + 'static,
    {
        // Only the variant is logged: the body of a token response may contain tokens.
        match error {
            RequestTokenError::ServerResponse(_) => {
                tracing::debug!("oidc token endpoint refused the code");
                AuthError::InvalidCredentials
            }
            RequestTokenError::Request(transport) => {
                self.inner.discovery.report_contact(false);
                tracing::warn!(error = %transport, "oidc token endpoint unreachable");
                unavailable("token endpoint request failed")
            }
            RequestTokenError::Parse(_, _) | RequestTokenError::Other(_) => {
                tracing::warn!("oidc token endpoint sent an unusable response");
                unavailable("token endpoint response unusable")
            }
        }
    }
}

/// Bounded memory of `state` values already used. Entries live as long as the cookie they
/// belong to, so they leave in insertion order: a queue plus a set is all it takes.
struct ConsumedStates {
    ttl: Duration,
    capacity: usize,
    order: VecDeque<(Instant, [u8; 32])>,
    seen: HashSet<[u8; 32]>,
}

impl ConsumedStates {
    fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            order: VecDeque::new(),
            seen: HashSet::new(),
        }
    }

    /// Records `key`; false when it was recorded before and has not expired.
    fn first_use(&mut self, key: [u8; 32], now: Instant) -> bool {
        while let Some(&(at, oldest)) = self.order.front() {
            if now.saturating_duration_since(at) < self.ttl && self.order.len() < self.capacity {
                break;
            }
            self.seen.remove(&oldest);
            self.order.pop_front();
        }
        if !self.seen.insert(key) {
            return false;
        }
        self.order.push_back((now, key));
        true
    }
}

/// Components the flow shares with the rest of the provider.
pub(crate) struct FlowParts {
    pub(crate) discovery: Arc<Discovery>,
    pub(crate) http: OidcHttp,
    pub(crate) validator: Arc<TokenValidator>,
}

fn unavailable(reason: &str) -> AuthError {
    AuthError::ProviderUnavailable(reason.to_owned())
}

fn backend(
    context: &'static str,
    error: impl std::error::Error + Send + Sync + 'static,
) -> AuthError {
    AuthError::Backend {
        context,
        source: Box::new(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumed_states_reject_a_repeat_until_it_expires() {
        let t0 = Instant::now();
        let mut states = ConsumedStates::new(Duration::from_secs(10), 100);
        assert!(states.first_use([1; 32], t0));
        assert!(!states.first_use([1; 32], t0 + Duration::from_secs(9)));
        assert!(states.first_use([2; 32], t0 + Duration::from_secs(9)));
        assert!(states.first_use([1; 32], t0 + Duration::from_secs(11)));
        assert!(!states.first_use([2; 32], t0 + Duration::from_secs(12)));
    }

    #[test]
    fn consumed_states_stay_within_capacity() {
        let t0 = Instant::now();
        let mut states = ConsumedStates::new(Duration::from_secs(600), 3);
        for n in 0..10u8 {
            assert!(states.first_use([n; 32], t0));
            assert!(states.order.len() <= 3 && states.seen.len() == states.order.len());
        }
        // The oldest entries were evicted, the newest are still remembered.
        assert!(!states.first_use([9; 32], t0));
        assert!(states.first_use([0; 32], t0));
    }
}
