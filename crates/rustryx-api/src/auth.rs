//! Credential extraction and the authentication middleware.
//!
//! The middleware turns the `Authorization` header into
//! [`Credentials`](rustryx_core::identity::Credentials), runs the authenticator
//! chain from [`AppState`] and stores the resulting [`Principal`] in the request
//! extensions. It makes no authorization decision: handlers (or
//! [`AppState::authorize`]) decide what the principal may do.
//!
//! Rules that hold on every path through this module:
//!
//! - No header means anonymous. A header that is present but unusable (unknown
//!   scheme, malformed, duplicated) is a 401, never anonymous: a client that tried
//!   to authenticate must not be silently downgraded to a weaker identity.
//! - Credentials are read from the `Authorization` header only. A token in the
//!   query string is ignored, because URLs end up in logs, proxies and history.
//! - Credentials presented while no authenticator is wired are a 503, never accepted.

use std::convert::Infallible;
use std::net::IpAddr;

use axum::extract::{FromRequestParts, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use http::header::AUTHORIZATION;
use http::request::Parts;
use rustryx_core::AuthError;
use rustryx_core::Secret;
use rustryx_core::identity::{AuthContext, AuthMethod, Credentials, Principal};
use rustryx_observability::MetricsHandle;
use tracing::Span;

use crate::client_ip::ClientIp;
use crate::error::ApiError;
use crate::request_id::RequestId;
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

/// Middleware: authenticates the request and stores the [`Principal`].
pub(crate) async fn authenticate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let (parts, body) = request.into_parts();
    match resolve(&state, &parts).await {
        Ok(principal) => {
            Span::current().record("user", principal.username.as_str());
            let mut request = Request::from_parts(parts, body);
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Err(mut error) => {
            if error.status() == http::StatusCode::UNAUTHORIZED && error.challenge().is_none() {
                error = error.with_challenge(state.challenge_for(&parts));
            }
            error.into_response()
        }
    }
}

async fn resolve(state: &AppState, parts: &Parts) -> Result<Principal, ApiError> {
    let credentials = match credentials_from_headers(&parts.headers) {
        Ok(Credentials::Anonymous) => return Ok(Principal::anonymous()),
        Ok(credentials) => credentials,
        Err(error) => {
            record_attempt(state.metrics(), None, Some(&error));
            return Err(error.into());
        }
    };
    let Some(authenticator) = state.authenticator() else {
        return Err(ApiError::unavailable("authentication_unavailable")
            .with_source("credentials were presented but no authenticator is wired".into()));
    };
    let context = AuthContext {
        client_ip: parts.extensions.get::<ClientIp>().and_then(|c| c.0),
        request_id: parts
            .extensions
            .get::<RequestId>()
            .map(|id| id.as_str().to_owned()),
    };
    let outcome = authenticator.authenticate(&credentials, &context).await;
    match outcome {
        Ok(Some(principal)) => {
            record_attempt(state.metrics(), Some(principal.auth_method), None);
            Ok(principal)
        }
        // No authenticator in the chain recognised the credentials.
        Ok(None) => {
            let error = AuthError::InvalidCredentials;
            record_attempt(state.metrics(), None, Some(&error));
            Err(error.into())
        }
        Err(error) => {
            record_attempt(state.metrics(), None, Some(&error));
            Err(error.into())
        }
    }
}

/// `auth_attempts_total{method,result}`. On failure the method is unknown (the
/// chain did not say which authenticator rejected), so it is `unknown` rather than
/// a guess from the header scheme.
fn record_attempt(
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

fn method_label(method: AuthMethod) -> &'static str {
    match method {
        AuthMethod::Anonymous => "anonymous",
        AuthMethod::BasicLocal => "local",
        AuthMethod::BasicLdap => "ldap",
        AuthMethod::ApiToken => "token",
        AuthMethod::InternalJwt => "internal_jwt",
        AuthMethod::OidcBearer => "oidc",
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
        | AuthError::UsernameConflict => "denied",
        AuthError::GroupLookupFailed(_)
        | AuthError::ProviderUnavailable(_)
        | AuthError::Overloaded
        | AuthError::GuardUnavailable
        | AuthError::Backend { .. } => "error",
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
            Err(ApiError::unauthenticated())
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
mod tests;
