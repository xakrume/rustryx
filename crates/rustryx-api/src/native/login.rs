//! Sign-in options, password login and logout.

use axum::Json;
use axum::extract::State;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use rustryx_core::audit::AuditKind;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{API_TOKEN_PREFIX, AuthMethod, Credentials, Principal};
use rustryx_core::secret::Secret;

use super::AuthApi;
use super::audit::Entry;
use super::dto::{Enabled, LoginProviders, LogoutResult, OidcProvider, SessionView, UserRef};
use super::requests::LoginRequest;
use crate::auth::{ActiveSession, append_cookies, record_attempt, record_attempt_for};
use crate::client_ip::ClientIp;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::https::is_https;
use crate::request_id::RequestId;
use crate::session::SessionManager;

/// Path of the SSO login, as the login page opens it (a top level navigation).
const OIDC_LOGIN_PATH: &str = "/api/v1/auth/oidc/login";

/// `GET /api/v1/auth/providers`.
pub(super) async fn providers(State(api): State<AuthApi>) -> Json<LoginProviders> {
    let oidc = api.oidc.as_ref();
    Json(LoginProviders {
        // The break-glass administrator is local, so the form is always there.
        local: Enabled { enabled: true },
        ldap: Enabled {
            enabled: api.settings.ldap_enabled,
        },
        oidc: OidcProvider {
            enabled: oidc.is_some(),
            available: oidc.is_some_and(|port| port.available()),
            display_name: oidc.and(api.settings.oidc_display_name.clone()),
            login_path: oidc.map(|_| OIDC_LOGIN_PATH.to_owned()),
        },
    })
}

pub(super) fn client_ip(parts: &Parts) -> Option<std::net::IpAddr> {
    parts.extensions.get::<ClientIp>().and_then(|c| c.0)
}

pub(super) fn sessions_or_unavailable(
    api: &AuthApi,
) -> Result<&std::sync::Arc<SessionManager>, ApiError> {
    api.app.sessions().ok_or_else(|| {
        ApiError::unavailable("authentication_unavailable")
            .with_source("no session manager is wired".into())
    })
}

/// The user of a session as the API reports it.
pub(super) fn user_ref(principal: &Principal) -> Option<UserRef> {
    Some(UserRef {
        id: principal.user_id?,
        username: principal.username.clone(),
        display_name: principal.display_name.clone(),
        source: principal.source,
    })
}

/// `POST /api/v1/auth/login`.
///
/// The `Origin` check is the CSRF defence of a form that has no session yet to carry a token. The credentials then go through the same chain as everything else (so the
/// login guard of `rustryx-auth` counts failures per account and per address), as a Basic
/// credential: this endpoint never accepts anything else as a password, an API token least of
/// all.
///
/// The HTTPS rule of HTTP Basic applies here too, and first: a password in a JSON body over plain
/// HTTP is as readable on the wire as one in an `Authorization` header, and this endpoint is the
/// one a browser form uses. The refusal comes before the Origin check and before any
/// authenticator is called, so it cannot be used to guess passwords either.
pub(super) async fn login(
    State(api): State<AuthApi>,
    parts: Parts,
    ApiJson(body): ApiJson<LoginRequest>,
) -> Result<Response, ApiError> {
    let settings = api.app.settings();
    if settings.require_https && !is_https(&parts, &settings.trusted_proxies) {
        tracing::info!("password login refused on a request that is not HTTPS");
        record_attempt(
            api.app.metrics(),
            None,
            Some(&AuthError::InvalidCredentials),
        );
        return Err(ApiError::https_required());
    }
    if !api.app.origins().permits(&parts.headers) {
        tracing::info!("login refused: the Origin is not the service origin");
        return Err(ApiError::csrf_rejected());
    }
    let sessions = sessions_or_unavailable(&api)?;
    let Some(authenticator) = api.app.authenticator() else {
        return Err(ApiError::unavailable("authentication_unavailable")
            .with_source("no authenticator is wired".into()));
    };
    let ip = client_ip(&parts);
    let fail = |error: AuthError| async {
        api.record_best_effort(Entry {
            kind: AuditKind::LoginFailure,
            action: "auth.login",
            success: false,
            actor_name: &body.username,
            actor_id: None,
            client_ip: ip,
            detail: serde_json::json!({}),
        })
        .await;
        ApiError::from(error)
    };
    // A password that starts with the token prefix would be routed to the token authenticator
    // by the chain: a token must not open a UI session, and a refusal here is the same
    // answer as for a wrong password.
    if body.password.starts_with(API_TOKEN_PREFIX) {
        return Err(fail(AuthError::InvalidCredentials).await);
    }
    let credentials = Credentials::Basic {
        username: body.username.clone(),
        password: Secret::new(body.password.clone()),
    };
    let mut context = rustryx_core::identity::AuthContext::new();
    if let Some(ip) = ip {
        context = context.with_client_ip(ip);
    }
    if let Some(id) = parts.extensions.get::<RequestId>() {
        context = context.with_request_id(id.as_str());
    }
    context = context.with_deadline(std::time::Instant::now() + api.app.settings().request_timeout);
    let principal = match authenticator
        .authenticate_detailed(&credentials, &context)
        .await
    {
        Ok(Some(principal))
            if matches!(
                principal.auth_method,
                AuthMethod::BasicLocal | AuthMethod::BasicLdap
            ) =>
        {
            principal
        }
        Ok(_) => return Err(fail(AuthError::InvalidCredentials).await),
        Err(failure) => {
            record_attempt_for(api.app.metrics(), failure.method, &failure.error);
            return Err(fail(failure.error).await);
        }
    };
    let Some(user_id) = principal.user_id else {
        return Err(fail(AuthError::InvalidCredentials).await);
    };
    record_attempt(api.app.metrics(), Some(principal.auth_method), None);
    let start = sessions.start(user_id, None, None)?;
    api.record(Entry {
        kind: AuditKind::LoginSuccess,
        action: "auth.login",
        success: true,
        actor_name: &principal.username,
        actor_id: Some(user_id),
        client_ip: ip,
        detail: serde_json::json!({"method": principal.auth_method}),
    })
    .await?;
    let times = sessions.times(&start.claims);
    let view = SessionView {
        authenticated: true,
        user: user_ref(&principal),
        auth_method: Some(AuthMethod::InternalJwt),
        csrf_token: Some(start.csrf_token.clone()),
        issued_at: Some(times.issued),
        idle_expires_at: Some(times.idle_end),
        absolute_expires_at: Some(times.absolute_end),
        idle_timeout_secs: sessions.settings().idle_timeout.as_secs(),
        absolute_timeout_secs: sessions.settings().absolute_timeout.as_secs(),
    };
    let mut response = Json(view).into_response();
    append_cookies(&mut response, &start.cookies);
    Ok(response)
}

/// `POST /api/v1/auth/logout`: idempotent. With a session it revokes the token id, so that a
/// copied cookie is useless; either way it clears the cookies.
///
/// A failure to revoke is logged and the cookies are still cleared: the user asked to leave, and
/// the browser must forget the session whatever happens to the revocation list.
pub(super) async fn logout(State(api): State<AuthApi>, parts: Parts) -> Result<Response, ApiError> {
    let sessions = sessions_or_unavailable(&api)?;
    if let Some(ActiveSession(session)) = parts.extensions.get::<ActiveSession>() {
        if let Err(error) = sessions.revoke(&session.claims).await {
            tracing::error!(%error, "session revocation failed during logout");
        }
        api.record_best_effort(Entry {
            kind: AuditKind::LoginSuccess,
            action: "auth.logout",
            success: true,
            actor_name: parts
                .extensions
                .get::<Principal>()
                .map_or("unknown", |p| p.username.as_str()),
            actor_id: Some(session.claims.user_id),
            client_ip: client_ip(&parts),
            detail: serde_json::json!({}),
        })
        .await;
    }
    let mut response = (StatusCode::OK, Json(LogoutResult { redirect_url: None })).into_response();
    append_cookies(&mut response, &SessionManager::clearing_cookies());
    Ok(response)
}

#[cfg(test)]
mod tests;
