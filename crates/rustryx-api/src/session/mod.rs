//! The cookie session of the web UI: issue, read, refresh and end.
//!
//! The browser holds two cookies, both `__Host-` prefixed (`Secure`, `Path=/`, no `Domain`):
//!
//! | cookie | content | flags |
//! |---|---|---|
//! | `__Host-rustryx_session` | the internal JWT | `HttpOnly`, `SameSite=Lax` (the OIDC callback is a cross-site top level navigation) |
//! | `__Host-rustryx_csrf` | the CSRF token of that token id | script readable, `SameSite=Strict` |
//!
//! An SSO session also carries `__Host-rustryx_sso`, an opaque value that only the SSO adapter
//! can read (see [`SsoSessionRefresher`](crate::SsoSessionRefresher)).
//!
//! # Limits and what enforces them
//!
//! - **Absolute, 8 hours at most** (`auth.session.absolute_timeout_secs`): the `exp` of the
//!   token. It is fixed when the session starts and a reissue keeps it, so nothing extends it:
//!   not activity, not the Keycloak session. The token service itself refuses an `exp - iat`
//!   above eight hours, so the bound holds even for a configuration mistake.
//! - **Idle, 30 minutes at most, sliding** (`auth.session.idle_timeout_secs`): `iat + idle`,
//!   where `iat` is the time of the last issue. A request past it is a session that ended. A
//!   request that finds less than half of the idle time left makes the server reissue the token
//!   in the same response (a `Set-Cookie`), which moves `iat`. Granularity is therefore half the
//!   idle time: a user active just before a reissue is due keeps at least half of it.
//! - **Group snapshot, 5 minutes** (SSO only): the chain refuses an older snapshot. A reissue
//!   after two minutes asks the SSO refresher for a new one; without a refresher the snapshot
//!   cannot be renewed and the session ends when it is five minutes old.
//!
//! - **Replaced tokens end.** A reissue revokes the token it replaced, after a short grace
//!   (`SessionSettings::revoke_grace`, 30 seconds by default) so that requests already in flight
//!   with the old cookie are not refused; see [`revocation`]. A logout ends the current token at
//!   once and, with it, any predecessor still waiting out its grace.
//!
//! The session token is never read here without the chain verifying it: [`claims::peek`] feeds
//! checks that come before the chain (idle, CSRF binding) and values for the reissue, and the
//! request is refused or downgraded to anonymous when the chain disagrees.

use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;
use rustryx_core::error::AuthError;
use rustryx_core::ids::UserId;
use rustryx_core::ports::Clock;
use rustryx_core::secret::Secret;
use subtle::ConstantTimeEq;
use time::OffsetDateTime;

use crate::error::ApiError;
use crate::ports::{
    GroupSnapshot, IssuedSession, SessionIssuer, SessionRequest, SsoRefreshRequest,
    SsoSessionRefresher,
};
use revocation::{Revocations, Stale};

pub(crate) mod claims;
mod cookies;
mod csrf;
mod revocation;

pub use csrf::{CsrfKey, CsrfKeyError};

pub(crate) use claims::SessionClaims;
pub(crate) use cookies::{Found, find as find_cookie, header_text as cookie_header_text};

/// The cookie that holds the session token.
pub const SESSION_COOKIE: &str = "__Host-rustryx_session";
/// The script readable cookie that holds the CSRF token.
pub const CSRF_COOKIE: &str = "__Host-rustryx_csrf";
/// The opaque SSO carrier cookie.
pub const SSO_COOKIE: &str = "__Host-rustryx_sso";
/// The request header that carries the CSRF token.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// The idle timeout may not exceed this (the requirement).
pub const MAX_IDLE_TIMEOUT: Duration = Duration::from_mins(30);
/// The absolute lifetime may not exceed this (the requirement).
pub const MAX_ABSOLUTE_TIMEOUT: Duration = Duration::from_hours(8);
/// An SSO snapshot older than this is renewed on the next reissue.
pub const GROUP_REFRESH_AFTER: Duration = Duration::from_mins(2);
/// The chain refuses an SSO snapshot older than this.
pub const MAX_GROUP_AGE: Duration = Duration::from_mins(5);
/// How long a token stays valid after a reissue replaced it (requests already in flight).
pub const DEFAULT_REVOKE_GRACE: Duration = Duration::from_secs(30);
/// The grace may not exceed this: a long grace would keep a stolen copy alive.
pub const MAX_REVOKE_GRACE: Duration = Duration::from_mins(5);

/// The two limits. Values above the requirement are clamped to it, so a configuration that
/// slipped past validation cannot give a session more than the maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionSettings {
    /// Idle timeout.
    pub idle_timeout: Duration,
    /// Absolute lifetime.
    pub absolute_timeout: Duration,
    /// How long the token a reissue replaced stays valid. See [`revocation`].
    pub revoke_grace: Duration,
}

impl SessionSettings {
    /// Settings with the requirement as the ceiling and the idle time never above the
    /// absolute time.
    #[must_use]
    pub fn new(idle_timeout: Duration, absolute_timeout: Duration) -> Self {
        let absolute_timeout = absolute_timeout.min(MAX_ABSOLUTE_TIMEOUT);
        Self {
            idle_timeout: idle_timeout.min(MAX_IDLE_TIMEOUT).min(absolute_timeout),
            absolute_timeout,
            revoke_grace: DEFAULT_REVOKE_GRACE,
        }
    }

    /// The same settings with another grace for replaced tokens, at most [`MAX_REVOKE_GRACE`].
    /// Zero revokes the old token as soon as the new one is issued.
    #[must_use]
    pub fn with_revoke_grace(mut self, grace: Duration) -> Self {
        self.revoke_grace = grace.min(MAX_REVOKE_GRACE);
        self
    }
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self::new(MAX_IDLE_TIMEOUT, MAX_ABSOLUTE_TIMEOUT)
    }
}

/// Why a presented session cookie is not a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionProblem {
    /// Not a session token at all, or ambiguous cookies.
    Malformed,
    /// Past its idle or absolute end, or its group snapshot can no longer be confirmed.
    Expired,
}

/// A session cookie as the browser sent it, parsed but not yet verified.
#[derive(Debug, Clone)]
pub(crate) struct CookieSession {
    /// The token, handed to the chain as a bearer credential.
    pub token: Secret,
    /// What the token claims (verified by the chain on the same request).
    pub claims: SessionClaims,
    /// The CSRF cookie, if the browser sent it.
    pub csrf_cookie: Option<String>,
    /// The SSO carrier cookie, if any.
    pub sso_carrier: Option<String>,
}

/// The times a client is told about its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionTimes {
    pub issued: OffsetDateTime,
    pub idle_end: OffsetDateTime,
    pub absolute_end: OffsetDateTime,
}

/// A session that was just started or reissued: the cookies to send and what to tell the client.
#[derive(Debug)]
pub(crate) struct SessionStart {
    /// The token, for a caller that wants to verify it through the chain right away.
    pub token: Secret,
    /// What the new token claims.
    pub claims: SessionClaims,
    /// Complete `Set-Cookie` values.
    pub cookies: Vec<String>,
    /// The CSRF token of the new token id.
    pub csrf_token: String,
}

/// Starts, reads, renews and ends sessions.
pub struct SessionManager {
    settings: SessionSettings,
    issuer: Arc<dyn SessionIssuer>,
    clock: Arc<dyn Clock>,
    csrf: CsrfKey,
    refresher: Option<Arc<dyn SsoSessionRefresher>>,
    revocations: Revocations,
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("settings", &self.settings)
            .field("sso_refresh", &self.refresher.is_some())
            .finish_non_exhaustive()
    }
}

fn seconds(duration: Duration) -> time::Duration {
    time::Duration::seconds(i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

/// 401 `token_expired`: the session ended (idle, absolute, or the groups can no longer be
/// confirmed).
pub(crate) fn session_expired() -> ApiError {
    ApiError::from(AuthError::TokenExpired)
}

/// Cookie value characters an adapter's carrier may use (base64url and a few separators).
fn cookie_safe(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'=')
        })
}

impl SessionManager {
    /// A manager. `refresher` is the SSO adapter's refresh hook, `None` while there is none.
    #[must_use]
    pub fn new(
        settings: SessionSettings,
        issuer: Arc<dyn SessionIssuer>,
        clock: Arc<dyn Clock>,
        csrf: CsrfKey,
        refresher: Option<Arc<dyn SsoSessionRefresher>>,
    ) -> Self {
        Self {
            settings,
            revocations: Revocations::new(Arc::clone(&issuer), settings.revoke_grace),
            issuer,
            clock,
            csrf,
            refresher,
        }
    }

    /// The limits in force.
    #[must_use]
    pub fn settings(&self) -> &SessionSettings {
        &self.settings
    }

    /// Reads the session cookie of a request. `None` when there is none.
    pub(crate) fn read(headers: &HeaderMap) -> Option<Result<CookieSession, SessionProblem>> {
        let token = match find_cookie(headers, SESSION_COOKIE) {
            Found::Absent => return None,
            Found::Ambiguous => return Some(Err(SessionProblem::Malformed)),
            Found::One(token) => token,
        };
        let Some(claims) = claims::peek(&token) else {
            return Some(Err(SessionProblem::Malformed));
        };
        let one = |name| match find_cookie(headers, name) {
            Found::One(value) => Some(value),
            _ => None,
        };
        Some(Ok(CookieSession {
            token: Secret::new(token),
            claims,
            csrf_cookie: one(CSRF_COOKIE),
            sso_carrier: one(SSO_COOKIE).filter(|v| cookie_safe(v)),
        }))
    }

    /// When the idle limit of a token ends: `iat + idle`, never past the absolute end.
    fn idle_deadline(&self, claims: &SessionClaims) -> OffsetDateTime {
        (claims.issued_at + seconds(self.settings.idle_timeout)).min(claims.expires_at)
    }

    /// The times to report for a token.
    pub(crate) fn times(&self, claims: &SessionClaims) -> SessionTimes {
        SessionTimes {
            issued: claims.issued_at,
            idle_end: self.idle_deadline(claims),
            absolute_end: claims.expires_at,
        }
    }

    /// Refuses a token that is past its idle or absolute end. Checked before the chain so that
    /// an idle session costs no database read.
    pub(crate) fn check_alive(&self, claims: &SessionClaims) -> Result<(), SessionProblem> {
        let now = self.clock.now();
        if now >= claims.expires_at || now >= self.idle_deadline(claims) {
            Err(SessionProblem::Expired)
        } else {
            Ok(())
        }
    }

    /// The CSRF token of a token id.
    pub(crate) fn csrf_token_for(&self, tid: &str) -> String {
        self.csrf.token_for(tid)
    }

    /// True when the request proves the CSRF token: header present, equal to the cookie, and the
    /// token of this session's own token id.
    pub(crate) fn csrf_accepts(&self, session: &CookieSession, header: Option<&str>) -> bool {
        let (Some(header), Some(cookie)) = (header, session.csrf_cookie.as_deref()) else {
            return false;
        };
        bool::from(header.as_bytes().ct_eq(cookie.as_bytes()))
            && self.csrf.verifies(&session.claims.tid, header)
    }

    /// True when a reissue is due: less than half of the idle time is left and a reissue would
    /// gain time, or an SSO snapshot is old enough to renew and a refresher can renew it.
    pub(crate) fn reissue_due(&self, session: &CookieSession) -> bool {
        let now = self.clock.now();
        let claims = &session.claims;
        let deadline = self.idle_deadline(claims);
        let target = (now + seconds(self.settings.idle_timeout)).min(claims.expires_at);
        let gain = target - deadline;
        let half = seconds(self.settings.idle_timeout) / 2;
        if (deadline - now) < half && gain >= time::Duration::SECOND {
            return true;
        }
        match (&claims.groups, &self.refresher, &session.sso_carrier) {
            (Some(snapshot), Some(_), Some(_)) => {
                now - snapshot.confirmed_at >= seconds(GROUP_REFRESH_AFTER)
            }
            _ => false,
        }
    }

    /// Starts a session for `user_id`. Its absolute end is `now + absolute_timeout`.
    ///
    /// # Errors
    /// The token service failed.
    pub(crate) fn start(
        &self,
        user_id: UserId,
        groups: Option<GroupSnapshot>,
        sso_carrier: Option<&str>,
    ) -> Result<SessionStart, ApiError> {
        let mut request = SessionRequest::new(user_id, self.settings.absolute_timeout);
        request.groups = groups;
        let issued = self.issuer.issue(&request)?;
        Ok(self.package(issued, user_id, request.groups, sso_carrier, true))
    }

    /// Reissues the token of a live session: a new `iat` (the idle timer restarts) and a new
    /// token id, the same `exp`. `strict` is the explicit refresh endpoint: a transient failure
    /// of the SSO refresh is then an error instead of being papered over with the old snapshot.
    ///
    /// # Errors
    /// 401 `token_expired` when the session ended or its groups cannot be confirmed again;
    /// the refresher's error when it failed and `strict` is set; the token service's otherwise.
    pub(crate) async fn renew(
        &self,
        session: &CookieSession,
        strict: bool,
    ) -> Result<SessionStart, ApiError> {
        let now = self.clock.now();
        let claims = &session.claims;
        let absolute_end = claims.expires_at;
        if self.check_alive(claims).is_err() || (absolute_end - now) < time::Duration::seconds(2) {
            return Err(session_expired());
        }
        let (groups, carrier) = self.renewed_groups(session, strict).await?;
        // The remaining lifetime in whole seconds, so that `exp` stays where it was.
        let remaining = u64::try_from((absolute_end - now).whole_seconds()).unwrap_or(0);
        let mut ttl = Duration::from_secs(remaining);
        let mut request = SessionRequest::new(claims.user_id, ttl);
        request.groups.clone_from(&groups);
        let mut issued = self.issuer.issue(&request)?;
        if issued.expires_at > absolute_end && remaining > 1 {
            // The token service read its clock a second later than this method did, and the
            // new `exp` would extend the session by that second. One second less is exact.
            ttl = Duration::from_secs(remaining - 1);
            request.ttl = ttl;
            issued = self.issuer.issue(&request)?;
        }
        if issued.expires_at > absolute_end {
            return Err(ApiError::internal(
                "a reissued session token would outlive the absolute limit",
            ));
        }
        let carrier = carrier.or_else(|| session.sso_carrier.clone());
        let start = self.package(issued, claims.user_id, groups, carrier.as_deref(), false);
        // Only now, with the new token in hand: a reissue that failed above leaves the old
        // token exactly as it was.
        self.revocations
            .retire(
                Stale {
                    user_id: claims.user_id,
                    tid: claims.tid.clone(),
                    until: claims.expires_at,
                },
                &start.claims.tid,
            )
            .await;
        Ok(start)
    }

    /// The group snapshot for a reissue, and a rotated carrier when the adapter made one.
    async fn renewed_groups(
        &self,
        session: &CookieSession,
        strict: bool,
    ) -> Result<(Option<GroupSnapshot>, Option<String>), ApiError> {
        let Some(snapshot) = &session.claims.groups else {
            return Ok((None, None));
        };
        let now = self.clock.now();
        let age = now - snapshot.confirmed_at;
        let refreshable = self.refresher.as_ref().zip(session.sso_carrier.as_deref());
        let due = age >= seconds(GROUP_REFRESH_AFTER) || strict;
        if let (true, Some((refresher, carrier))) = (due, refreshable) {
            let request = SsoRefreshRequest::new(session.claims.user_id, carrier);
            return match refresher.refresh(&request).await {
                Ok(refreshed) => Ok((Some(refreshed.snapshot), refreshed.carrier)),
                // The provider cannot be reached right now: keep serving on the old
                // snapshot while it is still young enough, except for the explicit endpoint.
                Err(
                    AuthError::ProviderUnavailable(_)
                    | AuthError::Overloaded
                    | AuthError::Backend { .. },
                ) if !strict && age <= seconds(MAX_GROUP_AGE) => Ok((Some(snapshot.clone()), None)),
                Err(error @ (AuthError::ProviderUnavailable(_) | AuthError::Overloaded)) => {
                    Err(error.into())
                }
                // Anything else means the provider no longer vouches for the user: the
                // session ends, with the code of an ended session.
                Err(error) => {
                    tracing::info!(%error, "sso session refresh refused, ending the session");
                    Err(session_expired())
                }
            };
        }
        if age > seconds(MAX_GROUP_AGE) {
            // The chain would refuse this snapshot on the next request anyway.
            return Err(session_expired());
        }
        Ok((Some(snapshot.clone()), None))
    }

    fn package(
        &self,
        issued: IssuedSession,
        user_id: UserId,
        groups: Option<GroupSnapshot>,
        sso_carrier: Option<&str>,
        fresh_login: bool,
    ) -> SessionStart {
        let now = self.clock.now();
        let remaining = (issued.expires_at - now).whole_seconds();
        let csrf_token = self.csrf.token_for(&issued.tid);
        let mut cookies = vec![
            cookies::SetCookie {
                name: SESSION_COOKIE,
                value: issued.token.expose(),
                max_age_secs: remaining,
                http_only: true,
                same_site: "Lax",
            }
            .render(),
            cookies::SetCookie {
                name: CSRF_COOKIE,
                value: &csrf_token,
                max_age_secs: remaining,
                http_only: false,
                same_site: "Strict",
            }
            .render(),
        ];
        match sso_carrier.filter(|carrier| cookie_safe(carrier)) {
            Some(carrier) => cookies.push(
                cookies::SetCookie {
                    name: SSO_COOKIE,
                    value: carrier,
                    max_age_secs: remaining,
                    http_only: true,
                    same_site: "Lax",
                }
                .render(),
            ),
            // A fresh login must not inherit the carrier of an earlier session.
            None if fresh_login => cookies.push(cookies::clearing(SSO_COOKIE, true, "Lax")),
            None => {}
        }
        SessionStart {
            claims: SessionClaims {
                user_id,
                tid: issued.tid,
                issued_at: issued.issued_at,
                expires_at: issued.expires_at,
                groups,
            },
            token: issued.token,
            cookies,
            csrf_token,
        }
    }

    /// The `Set-Cookie` value that (re)sets the CSRF cookie of a token, living as long as the
    /// token.
    pub(crate) fn csrf_cookie(&self, claims: &SessionClaims, csrf_token: &str) -> String {
        cookies::SetCookie {
            name: CSRF_COOKIE,
            value: csrf_token,
            max_age_secs: (claims.expires_at - self.clock.now()).whole_seconds(),
            http_only: false,
            same_site: "Strict",
        }
        .render()
    }

    /// `Set-Cookie` values that remove the session, CSRF and SSO cookies.
    pub(crate) fn clearing_cookies() -> Vec<String> {
        vec![
            cookies::clearing(SESSION_COOKIE, true, "Lax"),
            cookies::clearing(CSRF_COOKIE, false, "Strict"),
            cookies::clearing(SSO_COOKIE, true, "Lax"),
        ]
    }

    /// Ends one session so that its token is refused even if someone copied it. The token it
    /// replaced a moment ago, if it is still waiting out its grace, ends with it.
    ///
    /// # Errors
    /// The revocation of the current token failed (the predecessors are revoked regardless, and a
    /// failure there is only logged).
    pub(crate) async fn revoke(&self, claims: &SessionClaims) -> Result<(), AuthError> {
        let current = self
            .issuer
            .revoke(claims.user_id, &claims.tid, claims.expires_at)
            .await;
        self.revocations.revoke_predecessors_of(&claims.tid).await;
        current
    }

    /// Runs the revocations that are waiting out their grace now and waits for them, at most
    /// `deadline`. Call it at shutdown, after the listener stopped and before the database
    /// closes, so that no replaced token outlives the process. Returns how many did not finish.
    pub async fn drain(&self, deadline: Duration) -> usize {
        self.revocations.drain(deadline).await
    }

    /// Revocations waiting or running.
    #[cfg(test)]
    pub(crate) fn pending_revocations(&self) -> usize {
        self.revocations.pending()
    }
}

#[cfg(test)]
mod grace_tests;
#[cfg(test)]
mod tests;
