//! `GET /auth/session`, `POST /auth/session/refresh` and `GET /auth/csrf`.

use axum::Json;
use axum::extract::State;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use rustryx_core::identity::{AuthMethod, Principal};

use super::AuthApi;
use super::dto::{CsrfTokenView, SessionView};
use super::login::{sessions_or_unavailable, user_ref};
use crate::auth::{ActiveSession, append_cookies};
use crate::authz::{Authorized, InteractiveGrant};
use crate::error::ApiError;
use crate::session::SessionManager;

/// What a caller without a cookie session is told: no session, and the configured limits.
fn anonymous_view(sessions: &SessionManager) -> SessionView {
    SessionView {
        authenticated: false,
        user: None,
        auth_method: None,
        csrf_token: None,
        issued_at: None,
        idle_expires_at: None,
        absolute_expires_at: None,
        idle_timeout_secs: sessions.settings().idle_timeout.as_secs(),
        absolute_timeout_secs: sessions.settings().absolute_timeout.as_secs(),
    }
}

/// A caller authenticated by a bearer credential: authenticated, no CSRF token (a bearer client
/// must not send one), and the only time known is when the credential ends.
fn bearer_view(sessions: &SessionManager, principal: &Principal) -> SessionView {
    SessionView {
        authenticated: true,
        user: user_ref(principal),
        auth_method: Some(principal.auth_method),
        absolute_expires_at: principal.expires_at,
        ..anonymous_view(sessions)
    }
}

fn cookie_view(
    sessions: &SessionManager,
    principal: &Principal,
    claims: &crate::session::SessionClaims,
    csrf_token: String,
) -> SessionView {
    let times = sessions.times(claims);
    SessionView {
        authenticated: true,
        user: user_ref(principal),
        auth_method: Some(AuthMethod::InternalJwt),
        csrf_token: Some(csrf_token),
        issued_at: Some(times.issued),
        idle_expires_at: Some(times.idle_end),
        absolute_expires_at: Some(times.absolute_end),
        ..anonymous_view(sessions)
    }
}

/// `GET /api/v1/auth/session`: bootstraps the SPA. It never extends the session; it only makes
/// sure the browser holds the CSRF cookie of the token it is using.
pub(super) async fn session(
    State(api): State<AuthApi>,
    parts: Parts,
) -> Result<Response, ApiError> {
    let sessions = sessions_or_unavailable(&api)?;
    let principal = parts.extensions.get::<Principal>();
    let Some(principal) = principal.filter(|p| !p.is_anonymous()) else {
        return Ok(Json(anonymous_view(sessions)).into_response());
    };
    let Some(ActiveSession(active)) = parts.extensions.get::<ActiveSession>() else {
        return Ok(Json(bearer_view(sessions, principal)).into_response());
    };
    let csrf_token = sessions.csrf_token_for(&active.claims.tid);
    let view = cookie_view(sessions, principal, &active.claims, csrf_token.clone());
    let mut response = Json(view).into_response();
    if active.csrf_cookie.as_deref() != Some(csrf_token.as_str()) {
        append_cookies(
            &mut response,
            &[sessions.csrf_cookie(&active.claims, &csrf_token)],
        );
    }
    Ok(response)
}

/// `POST /api/v1/auth/session/refresh`: resets the idle timer, reissues the token and, for an
/// SSO session, asks the provider again for the groups. It never moves the absolute end.
/// A caller that does not use the cookie session has nothing to refresh and is told the state of
/// its credential.
pub(super) async fn refresh(
    State(api): State<AuthApi>,
    proof: Authorized<InteractiveGrant>,
    parts: Parts,
) -> Result<Response, ApiError> {
    let sessions = sessions_or_unavailable(&api)?;
    let principal = proof.principal();
    let Some(ActiveSession(active)) = parts.extensions.get::<ActiveSession>() else {
        return Ok(Json(bearer_view(sessions, principal)).into_response());
    };
    let start = sessions.renew(active, true).await?;
    let view = cookie_view(sessions, principal, &start.claims, start.csrf_token.clone());
    let mut response = Json(view).into_response();
    append_cookies(&mut response, &start.cookies);
    Ok(response)
}

/// `GET /api/v1/auth/csrf`: the CSRF token of the current session, to recover after a 403
/// `csrf_rejected`. Only a cookie session has one; a bearer credential is not subject to CSRF.
pub(super) async fn csrf(
    State(api): State<AuthApi>,
    _proof: Authorized<InteractiveGrant>,
    parts: Parts,
) -> Result<Response, ApiError> {
    let sessions = sessions_or_unavailable(&api)?;
    let Some(ActiveSession(active)) = parts.extensions.get::<ActiveSession>() else {
        return Err(ApiError::forbidden());
    };
    let token = sessions.csrf_token_for(&active.claims.tid);
    let mut response = Json(CsrfTokenView {
        csrf_token: token.clone(),
    })
    .into_response();
    append_cookies(
        &mut response,
        &[sessions.csrf_cookie(&active.claims, &token)],
    );
    Ok(response)
}

#[cfg(test)]
mod tests;
