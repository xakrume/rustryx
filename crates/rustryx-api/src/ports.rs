//! Ports of the cookie session and the SSO login.
//!
//! `rustryx-api` depends on `rustryx-core` only. Everything that needs the token service or
//! the Keycloak client of `rustryx-auth` is a trait here, implemented by an adapter in the
//! composition root. That keeps this crate testable with fakes and free of the auth crate's
//! build, and it is also the seam where the missing pieces of the SSO session (the refresh
//! token) plug in later without touching the HTTP layer.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::error::AuthError;
use rustryx_core::ids::UserId;
use rustryx_core::secret::Secret;
use time::OffsetDateTime;

/// The external groups an SSO login was confirmed with, carried inside the session token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSnapshot {
    /// Full group paths, byte for byte as the provider sent them.
    pub groups: Vec<String>,
    /// When the provider last confirmed the membership.
    pub confirmed_at: OffsetDateTime,
}

/// What the session layer asks the token service to sign.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SessionRequest {
    /// The account the session belongs to.
    pub user_id: UserId,
    /// Lifetime of the token, at most eight hours.
    pub ttl: Duration,
    /// Group snapshot of an SSO session, `None` for local and LDAP sessions.
    pub groups: Option<GroupSnapshot>,
}

impl SessionRequest {
    /// A request for `ttl` without a group snapshot.
    #[must_use]
    pub fn new(user_id: UserId, ttl: Duration) -> Self {
        Self {
            user_id,
            ttl,
            groups: None,
        }
    }

    /// Adds the group snapshot of an SSO session.
    #[must_use]
    pub fn with_groups(mut self, groups: GroupSnapshot) -> Self {
        self.groups = Some(groups);
        self
    }
}

/// A freshly signed session token.
#[derive(Debug)]
#[non_exhaustive]
pub struct IssuedSession {
    /// The compact token. Goes into the cookie and nowhere else.
    pub token: Secret,
    /// The token id (`tid`), needed to revoke this one session.
    pub tid: String,
    /// `iat`.
    pub issued_at: OffsetDateTime,
    /// `exp`.
    pub expires_at: OffsetDateTime,
}

impl IssuedSession {
    /// Builds the answer of a [`SessionIssuer`].
    #[must_use]
    pub fn new(
        token: Secret,
        tid: String,
        issued_at: OffsetDateTime,
        expires_at: OffsetDateTime,
    ) -> Self {
        Self {
            token,
            tid,
            issued_at,
            expires_at,
        }
    }
}

/// Signs and revokes session tokens (the internal JWT service).
#[async_trait]
pub trait SessionIssuer: Send + Sync {
    /// Signs a token. Synchronous: an Ed25519 signature takes microseconds.
    ///
    /// # Errors
    /// The signer failed, or the request asks for more than the token service allows.
    fn issue(&self, request: &SessionRequest) -> Result<IssuedSession, AuthError>;

    /// Ends one session: its `tid` is refused until `until` (its `exp`). Cancel safe.
    ///
    /// `user_id` is there for the caches: the authentication chain keeps a verified principal for
    /// up to five minutes and skips the revocation check while it does, so an implementation must
    /// also make every node forget that user's cached principals (the invalidation bus), or the
    /// session outlives its logout by the length of the cache.
    async fn revoke(
        &self,
        user_id: UserId,
        tid: &str,
        until: OffsetDateTime,
    ) -> Result<(), AuthError>;
}

/// Why a login redirect could not start, or the pieces to send.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OidcStart {
    /// Where to send the browser (`302 Location`).
    pub location: String,
    /// Complete `Set-Cookie` header values (the flow cookie and the `return_to` cookie).
    pub set_cookies: Vec<String>,
}

impl OidcStart {
    /// Builds the answer of [`OidcLoginPort::start`].
    #[must_use]
    pub fn new(location: String, set_cookies: Vec<String>) -> Self {
        Self {
            location,
            set_cookies,
        }
    }
}

/// The query of the callback and the raw `Cookie` header, which holds the flow cookie.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct OidcCallback<'a> {
    /// `code`.
    pub code: Option<&'a str>,
    /// `state`.
    pub state: Option<&'a str>,
    /// `error`, set when Keycloak refused the login.
    pub error: Option<&'a str>,
    /// The request's `Cookie` header text (empty when absent).
    pub cookie_header: &'a str,
}

impl<'a> OidcCallback<'a> {
    /// A callback with a code and a state.
    #[must_use]
    pub fn new(code: Option<&'a str>, state: Option<&'a str>, cookie_header: &'a str) -> Self {
        Self {
            code,
            state,
            error: None,
            cookie_header,
        }
    }

    /// Marks the callback as an error answer of the provider.
    #[must_use]
    pub fn with_error(mut self, error: &'a str) -> Self {
        self.error = Some(error);
        self
    }
}

/// The account an SSO login produced.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OidcLogin {
    /// The account, provisioned (JIT) by the adapter.
    pub user_id: UserId,
    /// The groups the login was confirmed with.
    pub snapshot: GroupSnapshot,
    /// Opaque state the adapter wants back when the session is refreshed (a sealed Keycloak
    /// refresh token). `None` while the adapter has nothing to refresh with.
    pub sso_carrier: Option<String>,
}

impl OidcLogin {
    /// Builds the account of an SSO login.
    #[must_use]
    pub fn new(user_id: UserId, snapshot: GroupSnapshot) -> Self {
        Self {
            user_id,
            snapshot,
            sso_carrier: None,
        }
    }

    /// Adds the carrier for later refreshes.
    #[must_use]
    pub fn with_carrier(mut self, carrier: String) -> Self {
        self.sso_carrier = Some(carrier);
        self
    }
}

/// A finished callback.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OidcCompletion {
    /// The account and its groups.
    pub login: OidcLogin,
    /// Where to send the browser next, as stored when the login started.
    pub return_to: String,
    /// `Set-Cookie` values that remove the flow cookies.
    pub clear_cookies: Vec<String>,
}

impl OidcCompletion {
    /// Builds the answer of [`OidcLoginPort::complete`].
    #[must_use]
    pub fn new(login: OidcLogin, return_to: String, clear_cookies: Vec<String>) -> Self {
        Self {
            login,
            return_to,
            clear_cookies,
        }
    }
}

/// The interactive OIDC login (authorization code with PKCE) as the HTTP layer needs it.
#[async_trait]
pub trait OidcLoginPort: Send + Sync {
    /// False while discovery has not reached Keycloak (the login button is disabled).
    fn available(&self) -> bool;

    /// Starts a login. `return_to` is already validated as a plain absolute path.
    ///
    /// # Errors
    /// `ProviderUnavailable` while Keycloak cannot be reached.
    fn start(&self, return_to: &str) -> Result<OidcStart, AuthError>;

    /// Finishes a login. Errors follow `rustryx-auth`: `InvalidToken` for a bad cookie, state
    /// or ID token, `InvalidCredentials` when Keycloak refused the code, `ProviderUnavailable`,
    /// `UsernameConflict`, `AccessDenied`.
    async fn complete(&self, callback: &OidcCallback<'_>) -> Result<OidcCompletion, AuthError>;

    /// `Set-Cookie` values that remove the flow cookies, sent with every callback answer.
    fn clear_cookies(&self) -> Vec<String>;
}

/// What a session refresh asks of the SSO adapter.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct SsoRefreshRequest<'a> {
    /// The account of the session.
    pub user_id: UserId,
    /// The carrier the adapter handed out at login (or at the previous refresh).
    pub carrier: &'a str,
}

impl<'a> SsoRefreshRequest<'a> {
    /// Builds a request.
    #[must_use]
    pub fn new(user_id: UserId, carrier: &'a str) -> Self {
        Self { user_id, carrier }
    }
}

/// A refreshed SSO confirmation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SsoRefreshed {
    /// The groups, confirmed now.
    pub snapshot: GroupSnapshot,
    /// A new carrier when the adapter rotated its state (refresh tokens rotate).
    pub carrier: Option<String>,
}

impl SsoRefreshed {
    /// Builds the answer of [`SsoSessionRefresher::refresh`].
    #[must_use]
    pub fn new(snapshot: GroupSnapshot) -> Self {
        Self {
            snapshot,
            carrier: None,
        }
    }

    /// Adds the rotated carrier.
    #[must_use]
    pub fn with_carrier(mut self, carrier: String) -> Self {
        self.carrier = Some(carrier);
        self
    }
}

/// Confirms an SSO session with Keycloak again (the refresh token grant), so that the group
/// snapshot in the session token stays younger than five minutes while the user is active.
///
/// Without an implementation an SSO session ends five minutes after login: the chain refuses
/// an older snapshot. An `Err` ends the session (401 `token_expired`): the user was removed
/// from the group, or Keycloak refused the refresh token.
#[async_trait]
pub trait SsoSessionRefresher: Send + Sync {
    /// Cancel safe.
    async fn refresh(&self, request: &SsoRefreshRequest<'_>) -> Result<SsoRefreshed, AuthError>;
}
