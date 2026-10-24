//! Credential extraction and the authentication middleware.
//!
//! The middleware turns the request's credential into
//! [`Credentials`](rustryx_core::identity::Credentials), runs the authenticator chain from
//! [`AppState`] and stores the resulting [`Principal`] in the request extensions. It makes no
//! authorization decision: handlers (or [`AppState::authorize`]) decide what the principal may
//! do.
//!
//! The credential is one of two things, never both:
//!
//! 1. an `Authorization` header (Basic or Bearer), or
//! 2. when there is no such header, the session cookie of the web UI (see `session/`).
//!
//! Rules that hold on every path through this module:
//!
//! - No credential means anonymous. A header that is present but unusable (unknown scheme,
//!   malformed, duplicated) is a 401, never anonymous: a client that tried to authenticate must
//!   not be silently downgraded to a weaker identity.
//! - Credentials are read from the `Authorization` header and the session cookie only. A token
//!   in the query string is ignored, because URLs end up in logs, proxies and history.
//! - Credentials presented while no authenticator is wired are a 503, never accepted.
//! - HTTP Basic is refused unless the request is HTTPS (`server.require_https`): 403
//!   `https_required`, and the authenticator is not called, so a refused request cannot be used
//!   to test a password.
//! - A session cookie that is stale (idle, absolute, revoked, malformed) makes the request
//!   anonymous, not a 401: the browser sends the cookie to the login page too, and a dead
//!   cookie must not lock the user out of signing in again. Handlers that need a caller answer
//!   `token_expired` instead of `unauthenticated` when this happened.
//! - A request that the cookie authenticates and that can change state (anything but GET, HEAD,
//!   OPTIONS, TRACE) must carry the CSRF token and a service `Origin`, or it is a 403
//!   `csrf_rejected` and nothing runs. A bearer credential is not subject to this, and a request
//!   that carries both an `Authorization` header and a cookie ignores the cookie.
//! - A dependency failure while verifying a session (database, identity provider) is a 503 and
//!   never turns an authenticated user into an anonymous one.

use std::convert::Infallible;
use std::net::IpAddr;
use std::time::Instant;

use axum::extract::{FromRequestParts, MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use http::header::{AUTHORIZATION, SET_COOKIE};
use http::request::Parts;
use http::{HeaderValue, Method, StatusCode};
use rustryx_core::AuthError;
use rustryx_core::Secret;
use rustryx_core::identity::{AuthContext, AuthMethod, Credentials, Principal};
use rustryx_observability::MetricsHandle;
use tracing::Span;

use crate::client_ip::ClientIp;
use crate::error::ApiError;
use crate::https::is_https;
use crate::request_id::RequestId;
use crate::session::{CSRF_HEADER, CookieSession, SESSION_COOKIE, SessionManager, SessionProblem};
use crate::state::AppState;

/// Longest `Authorization` header value accepted. A Keycloak access token with a
/// long `groups` claim is a few kilobytes; anything near this limit is abuse.
const MAX_AUTHORIZATION_BYTES: usize = 16 * 1024;
const MAX_USERNAME_BYTES: usize = 256;
const MAX_PASSWORD_BYTES: usize = 4096;

/// Many clients drop the `=` padding of the Basic payload, so padding is optional.
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Parses the `Authorization` header(s) of a request.
///
/// # Errors
/// [`AuthError::InvalidCredentials`] for anything that is present but unusable.
pub fn credentials_from_headers(headers: &http::HeaderMap) -> Result<Credentials, AuthError> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let Some(first) = values.next() else {
        return Ok(Credentials::Anonymous);
    };
    if values.next().is_some() {
        return Err(AuthError::InvalidCredentials);
    }
    if first.len() > MAX_AUTHORIZATION_BYTES {
        return Err(AuthError::InvalidCredentials);
    }
    let text = first.to_str().map_err(|_| AuthError::InvalidCredentials)?;
    let (scheme, rest) = text.split_once(' ').ok_or(AuthError::InvalidCredentials)?;
    let rest = rest.trim_matches(' ');
    if scheme.eq_ignore_ascii_case("basic") {
        parse_basic(rest)
    } else if scheme.eq_ignore_ascii_case("bearer") {
        parse_bearer(rest)
    } else {
        Err(AuthError::InvalidCredentials)
    }
}

fn parse_basic(payload: &str) -> Result<Credentials, AuthError> {
    let decoded = LENIENT_BASE64
        .decode(payload)
        .map_err(|_| AuthError::InvalidCredentials)?;
    let text = String::from_utf8(decoded).map_err(|_| AuthError::InvalidCredentials)?;
    let (username, password) = text.split_once(':').ok_or(AuthError::InvalidCredentials)?;
    // An empty or NUL-bearing password is refused here as well as in the LDAP and
    // local authenticators: for LDAP an empty password makes a simple bind
    // anonymous, which some servers accept as success.
    let unusable = password.trim().is_empty()
        || username.len() > MAX_USERNAME_BYTES
        || password.len() > MAX_PASSWORD_BYTES
        || username.contains('\0')
        || password.contains('\0');
    if unusable {
        return Err(AuthError::InvalidCredentials);
    }
    Ok(Credentials::Basic {
        username: username.to_owned(),
        password: Secret::new(password),
    })
}

/// RFC 6750 `b64token`: `[A-Za-z0-9._~+/=-]+`.
fn parse_bearer(token: &str) -> Result<Credentials, AuthError> {
    let valid = !token.is_empty()
        && token.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
        });
    if valid {
        Ok(Credentials::Bearer(Secret::new(token)))
    } else {
        Err(AuthError::InvalidCredentials)
    }
}

/// Why a request that presented a session cookie is nevertheless anonymous. Kept in the
/// request extensions so that a handler that needs a caller can say `token_expired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionLost(pub(crate) SessionProblem);

/// The session that authenticated this request (cookie path only).
#[derive(Debug, Clone)]
pub(crate) struct ActiveSession(pub(crate) CookieSession);

/// What the credential of a request resolved to.
struct Resolved {
    principal: Principal,
    session: Option<CookieSession>,
    lost: Option<SessionProblem>,
}

impl Resolved {
    fn anonymous() -> Self {
        Self {
            principal: Principal::anonymous(),
            session: None,
            lost: None,
        }
    }

    fn lost(problem: SessionProblem) -> Self {
        Self {
            lost: Some(problem),
            ..Self::anonymous()
        }
    }

    fn named(principal: Principal) -> Self {
        Self {
            principal,
            session: None,
            lost: None,
        }
    }
}

/// Middleware: authenticates the request and stores the [`Principal`].
pub(crate) async fn authenticate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    match resolve(&state, &parts).await {
        Ok(Resolved {
            principal,
            session,
            lost,
        }) => {
            Span::current().record("user", principal.username.as_str());
            parts.extensions.insert(principal);
            if let Some(problem) = lost {
                parts.extensions.insert(SessionLost(problem));
            }
            if let Some(session) = &session {
                parts.extensions.insert(ActiveSession(session.clone()));
            }
            let mut response = next.run(Request::from_parts(parts, body)).await;
            if let Some(session) = session {
                reissue_if_due(&state, &session, &mut response).await;
            }
            response
        }
        Err(mut error) => {
            if error.status() == StatusCode::UNAUTHORIZED && error.challenge().is_none() {
                error = error.with_challenge(state.challenge_for(&parts));
            }
            error.into_response()
        }
    }
}

fn auth_context(state: &AppState, parts: &Parts) -> AuthContext {
    let mut context = AuthContext::new();
    if let Some(ip) = parts.extensions.get::<ClientIp>().and_then(|c| c.0) {
        context = context.with_client_ip(ip);
    }
    if let Some(id) = parts.extensions.get::<RequestId>() {
        context = context.with_request_id(id.as_str());
    }
    // The request's own deadline: a blocking-work gate must not wait past the time the
    // request is allowed to take.
    context.with_deadline(Instant::now() + state.settings().request_timeout)
}

async fn resolve(state: &AppState, parts: &Parts) -> Result<Resolved, ApiError> {
    let credentials = match credentials_from_headers(&parts.headers) {
        Ok(Credentials::Anonymous) => return resolve_cookie(state, parts).await,
        Ok(credentials) => credentials,
        Err(error) => {
            record_attempt(state.metrics(), None, Some(&error));
            return Err(error.into());
        }
    };
    if matches!(credentials, Credentials::Basic { .. })
        && state.settings().require_https
        && !is_https(parts, &state.settings().trusted_proxies)
    {
        // Refused before any authenticator sees the password: over plain HTTP it was already
        // readable on the wire, and answering "wrong password" would help whoever guesses.
        tracing::info!("HTTP Basic credentials refused on a request that is not HTTPS");
        record_attempt(state.metrics(), None, Some(&AuthError::InvalidCredentials));
        return Err(ApiError::https_required());
    }
    let Some(authenticator) = state.authenticator() else {
        return Err(ApiError::unavailable("authentication_unavailable")
            .with_source("credentials were presented but no authenticator is wired".into()));
    };
    let context = auth_context(state, parts);
    match authenticator
        .authenticate_detailed(&credentials, &context)
        .await
    {
        Ok(Some(principal)) => {
            record_attempt(state.metrics(), Some(principal.auth_method), None);
            Ok(Resolved::named(principal))
        }
        // No authenticator in the chain recognised the credentials. On `/v2` a bearer may
        // still be a registry token, which this chain does not speak. Anywhere else, and for
        // a bearer the registry hook refuses, the client is not anonymous.
        Ok(None) => {
            if let Some(principal) = registry_bearer(state, parts, &credentials).await? {
                record_attempt(state.metrics(), Some(principal.auth_method), None);
                return Ok(Resolved::named(principal));
            }
            let error = AuthError::InvalidCredentials;
            record_attempt(state.metrics(), None, Some(&error));
            Err(error.into())
        }
        Err(failure) => {
            record_attempt_for(state.metrics(), failure.method, &failure.error);
            Err(failure.error.into())
        }
    }
}

/// `/v2` and `/v2/...`: the distribution API, where a registry bearer is a credential.
fn is_distribution_api(parts: &Parts) -> bool {
    let path = parts.uri.path();
    path == "/v2" || path.starts_with("/v2/")
}

/// A bearer the chain left unclaimed, verified by the registry hook. `Ok(None)` when this
/// request is not a distribution call, carries no bearer, or no hook is wired: the caller
/// then refuses the credential as usual.
async fn registry_bearer(
    state: &AppState,
    parts: &Parts,
    credentials: &Credentials,
) -> Result<Option<Principal>, ApiError> {
    let Credentials::Bearer(secret) = credentials else {
        return Ok(None);
    };
    if !is_distribution_api(parts) {
        return Ok(None);
    }
    let Some(registry) = state.registry_bearer() else {
        return Ok(None);
    };
    match registry.accept(secret.expose()).await {
        Ok(principal) => Ok(Some(principal)),
        Err(error) => {
            record_attempt(state.metrics(), None, Some(&error));
            Err(error.into())
        }
    }
}

fn is_safe_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// The route that establishes a session from a username and a password. A browser that still
/// holds an older session cookie sends it along; that cookie is not what this request
/// authenticates with, so it is not asked for a CSRF token. The login form has its own defence
/// against forged requests: the `Origin` check of the handler.
const LOGIN_PATH: &str = "/api/v1/auth/login";

fn establishes_a_session(parts: &Parts) -> bool {
    parts
        .extensions
        .get::<MatchedPath>()
        .is_some_and(|path| path.as_str() == LOGIN_PATH)
}

/// The session cookie as a credential.
async fn resolve_cookie(state: &AppState, parts: &Parts) -> Result<Resolved, ApiError> {
    let Some(sessions) = state.sessions() else {
        return Ok(Resolved::anonymous());
    };
    if establishes_a_session(parts) {
        return Ok(Resolved::anonymous());
    }
    let session = match SessionManager::read(&parts.headers) {
        None => return Ok(Resolved::anonymous()),
        Some(Err(problem)) => return Ok(Resolved::lost(problem)),
        Some(Ok(session)) => session,
    };
    // Idle and absolute limits first: an expired session costs no database read, and a stale
    // cookie never reaches the CSRF check (it is not a credential any more).
    if let Err(problem) = sessions.check_alive(&session.claims) {
        return Ok(Resolved::lost(problem));
    }
    if !is_safe_method(&parts.method) {
        let header = parts
            .headers
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok());
        if !sessions.csrf_accepts(&session, header) || !state.origins().permits(&parts.headers) {
            tracing::info!("cookie session request without a valid CSRF token or service origin");
            return Err(ApiError::csrf_rejected());
        }
    }
    verify_session(state, parts, session).await
}

/// The chain verifies the token: signature, issuer, lifetime, account state, revocation and the
/// group snapshot of an SSO session.
async fn verify_session(
    state: &AppState,
    parts: &Parts,
    session: CookieSession,
) -> Result<Resolved, ApiError> {
    let Some(authenticator) = state.authenticator() else {
        return Err(ApiError::unavailable("authentication_unavailable")
            .with_source("a session cookie was presented but no authenticator is wired".into()));
    };
    let credentials = Credentials::Bearer(session.token.clone());
    let context = auth_context(state, parts);
    match authenticator
        .authenticate_detailed(&credentials, &context)
        .await
    {
        Ok(Some(principal)) if principal.auth_method == AuthMethod::InternalJwt => {
            record_attempt(state.metrics(), Some(principal.auth_method), None);
            Ok(Resolved {
                principal,
                session: Some(session),
                lost: None,
            })
        }
        // Whatever the chain made of the cookie, an internal session token is what it must be.
        Ok(_) => Ok(Resolved::lost(SessionProblem::Malformed)),
        Err(failure) => {
            record_attempt_for(state.metrics(), failure.method, &failure.error);
            match failure.error {
                // Revoked, expired, unknown, disabled, or the groups can no longer be
                // confirmed: the session is over, the request is anonymous.
                AuthError::TokenExpired
                | AuthError::TokenRevoked
                | AuthError::AccountDisabled
                | AuthError::GroupLookupFailed(_) => Ok(Resolved::lost(SessionProblem::Expired)),
                AuthError::InvalidToken(_) | AuthError::InvalidCredentials => {
                    Ok(Resolved::lost(SessionProblem::Malformed))
                }
                other => Err(other.into()),
            }
        }
    }
}

fn sets_session_cookie(response: &Response) -> bool {
    let prefix = format!("{SESSION_COOKIE}=");
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .any(|value| value.to_str().is_ok_and(|v| v.starts_with(&prefix)))
}

/// The transparent reissue: when less than half of the idle time is left, or an SSO snapshot
/// is old, the response carries a fresh token and CSRF cookie. A failure is logged and
/// ignored: the token the request came with is still valid.
async fn reissue_if_due(state: &AppState, session: &CookieSession, response: &mut Response) {
    let Some(sessions) = state.sessions() else {
        return;
    };
    // The handler dealt with the cookies itself (login, refresh, logout), or refused the caller.
    if response.status() == StatusCode::UNAUTHORIZED || sets_session_cookie(response) {
        return;
    }
    if !sessions.reissue_due(session) {
        return;
    }
    match sessions.renew(session, false).await {
        Ok(start) => append_cookies(response, &start.cookies),
        Err(error) => tracing::debug!(%error, "transparent session reissue skipped"),
    }
}

/// Adds `Set-Cookie` headers for complete cookie values.
pub(crate) fn append_cookies(response: &mut Response, cookies: &[String]) {
    for cookie in cookies {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            response.headers_mut().append(SET_COOKIE, value);
        } else {
            // Every value is built from token characters; this is a bug, not a request fault.
            tracing::error!("a Set-Cookie value was not a valid header value");
        }
    }
}

/// `auth_attempts_total{method,result}`. On failure the method is the one the chain named, or
/// `unknown` when it could not tell (a malformed header, a credential no member claimed).
pub(crate) fn record_attempt(
    metrics: Option<&MetricsHandle>,
    success: Option<AuthMethod>,
    failure: Option<&AuthError>,
) {
    let Some(metrics) = metrics else {
        return;
    };
    match (success, failure) {
        (Some(method), _) => metrics.record_auth_attempt(method_label(method), "ok"),
        (None, Some(error)) => metrics.record_auth_attempt("unknown", failure_label(error)),
        (None, None) => {}
    }
}

pub(crate) fn record_attempt_for(
    metrics: Option<&MetricsHandle>,
    method: Option<AuthMethod>,
    error: &AuthError,
) {
    let Some(metrics) = metrics else {
        return;
    };
    metrics.record_auth_attempt(method.map_or("unknown", method_label), failure_label(error));
}

fn method_label(method: AuthMethod) -> &'static str {
    match method {
        AuthMethod::Anonymous => "anonymous",
        AuthMethod::BasicLocal => "local",
        AuthMethod::BasicLdap => "ldap",
        AuthMethod::ApiToken => "token",
        AuthMethod::InternalJwt => "internal_jwt",
        AuthMethod::OidcBearer => "oidc",
        AuthMethod::RobotToken => "robot",
        AuthMethod::ScanToken => "scan",
        // A method added later is counted, under a label nobody alerts on, until it is named.
        _ => "unknown",
    }
}

/// "denied" is the caller's fault, "error" is ours or a dependency's.
fn failure_label(error: &AuthError) -> &'static str {
    match error {
        AuthError::InvalidCredentials
        | AuthError::InvalidToken(_)
        | AuthError::TokenExpired
        | AuthError::TokenRevoked
        | AuthError::AccountDisabled
        | AuthError::LockedOut { .. }
        | AuthError::UsernameConflict
        | AuthError::AccessDenied(_) => "denied",
        // A failed lookup, an outage, overload, `Backend` and every error added later.
        _ => "error",
    }
}

/// The 401 a handler answers when it needs a caller and there is none: `token_expired` when the
/// request presented a session that has ended, `unauthenticated` otherwise.
pub(crate) fn unauthenticated_for(extensions: &http::Extensions) -> ApiError {
    match extensions.get::<SessionLost>() {
        Some(SessionLost(SessionProblem::Expired)) => ApiError::from(AuthError::TokenExpired),
        _ => ApiError::unauthenticated(),
    }
}

/// The principal of this request. Handlers that need to know who is calling take
/// this extractor; it is anonymous when no credentials were presented.
#[derive(Debug, Clone)]
pub struct RequestPrincipal(pub Principal);

impl<S: Send + Sync> FromRequestParts<S> for RequestPrincipal {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<Principal>()
                .cloned()
                .map(Self)
                // Reaching a handler without the middleware is a wiring bug; failing
                // the request is safer than guessing an identity.
                .ok_or_else(|| {
                    ApiError::internal(
                        "no principal in request extensions: the authentication middleware is not applied to this route",
                    )
                }),
        )
    }
}

/// Like [`RequestPrincipal`] but rejects anonymous callers with 401.
#[derive(Debug, Clone)]
pub struct AuthenticatedPrincipal(pub Principal);

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedPrincipal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let RequestPrincipal(principal) =
            RequestPrincipal::from_request_parts(parts, state).await?;
        if principal.is_anonymous() {
            Err(unauthenticated_for(&parts.extensions))
        } else {
            Ok(Self(principal))
        }
    }
}

/// The client address resolved for this request (`None` without a peer).
#[derive(Debug, Clone, Copy)]
pub struct RequestClientIp(pub Option<IpAddr>);

impl<S: Send + Sync> FromRequestParts<S> for RequestClientIp {
    type Rejection = Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self(
            parts.extensions.get::<ClientIp>().and_then(|c| c.0),
        )))
    }
}

#[cfg(test)]
mod cookie_tests;
#[cfg(test)]
mod tests;
