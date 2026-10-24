//! The OIDC redirect flow: start and callback.
//!
//! `rustryx-auth` does the protocol (PKCE, state, nonce, the code exchange, the ID token). This
//! file turns its outcomes into HTTP and, at the end, into a cookie session. The adapter behind
//! [`OidcLoginPort`](crate::OidcLoginPort) owns every cookie of the flow (the sealed state cookie
//! and the one that holds `return_to`), so nothing here knows how they look.

use axum::extract::{Query, State};
use axum::http::header::{ACCEPT, LOCATION};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode};
use rustryx_core::audit::AuditKind;
use rustryx_core::error::AuthError;
use rustryx_core::identity::{AuthContext, AuthMethod, Credentials};

use super::AuthApi;
use super::audit::Entry;
use super::login::{client_ip, sessions_or_unavailable};
use super::requests::{OidcCallbackQuery, OidcLoginQuery};
use crate::auth::{append_cookies, record_attempt_for};
use crate::error::ApiError;
use crate::ports::{OidcCallback, OidcLoginPort};
use crate::request_id::{RequestId, current_request_id};
use crate::session::cookie_header_text;

/// Longest `return_to` accepted.
const MAX_RETURN_TO_BYTES: usize = 512;

/// A `return_to` that is a plain absolute path of this origin, or `/`.
///
/// Open redirect protection: `//host`, `/\host`, a scheme, a control character, a backslash or
/// an over long value would send the user (with a fresh session) to a page the attacker chose.
/// The value is checked again when the callback redirects to it, because it travelled through
/// the browser in between.
#[must_use]
pub(crate) fn sanitize_return_to(candidate: Option<&str>) -> String {
    let Some(path) = candidate else {
        return "/".to_owned();
    };
    let plain = path.len() <= MAX_RETURN_TO_BYTES
        && path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_control() || c == '\u{7f}')
        && path.is_ascii();
    if plain {
        path.to_owned()
    } else {
        "/".to_owned()
    }
}

fn oidc_port(api: &AuthApi) -> Result<&std::sync::Arc<dyn OidcLoginPort>, ApiError> {
    api.oidc.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "The resource was not found",
        )
    })
}

fn redirect(location: &str, cookies: &[String]) -> Result<Response, ApiError> {
    let value = HeaderValue::from_str(location)
        .map_err(|_| ApiError::internal("a redirect location is not a valid header value"))?;
    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(LOCATION, value);
    append_cookies(&mut response, cookies);
    Ok(response)
}

/// `GET /api/v1/auth/oidc/login`.
pub(super) async fn start(
    State(api): State<AuthApi>,
    Query(query): Query<OidcLoginQuery>,
) -> Result<Response, ApiError> {
    let port = oidc_port(&api)?;
    let return_to = sanitize_return_to(query.return_to.as_deref());
    let start = port.start(&return_to)?;
    redirect(&start.location, &start.set_cookies)
}

/// True when the client wants a page: `text/html` is accepted and ranks no lower than JSON.
fn prefers_html(parts: &Parts) -> bool {
    let Some(accept) = parts.headers.get(ACCEPT).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let position = |needle: &str| accept.to_ascii_lowercase().find(needle);
    match (position("text/html"), position("application/json")) {
        (Some(html), Some(json)) => html < json,
        (Some(_), None) => true,
        _ => false,
    }
}

/// A failed callback: a redirect to the login page for a browser, the envelope for anyone else.
/// The flow cookies are cleared either way.
fn failure(parts: &Parts, error: ApiError, clear: &[String]) -> Response {
    if prefers_html(parts) {
        let request_id = current_request_id()
            .map(|id| id.as_str().to_owned())
            .unwrap_or_default();
        let location = format!("/login?error={}&request_id={request_id}", error.code());
        match redirect(&location, clear) {
            Ok(response) => return response,
            Err(rejected) => return rejected.into_response(),
        }
    }
    let mut response = error.into_response();
    append_cookies(&mut response, clear);
    response
}

/// Maps what the SSO adapter reports to the codes of the contract.
fn callback_error(error: AuthError) -> ApiError {
    match error {
        // A bad cookie, state or ID token, or a callback that carries an error of Keycloak's.
        AuthError::InvalidToken(_) => ApiError::bad_request(
            "oidc_callback_invalid",
            "The sign-in could not be completed, start it again",
        )
        .with_source(Box::new(error)),
        other => ApiError::from(other),
    }
}

/// `GET /api/v1/auth/oidc/callback`.
pub(super) async fn callback(
    State(api): State<AuthApi>,
    parts: Parts,
    Query(query): Query<OidcCallbackQuery>,
) -> Result<Response, ApiError> {
    let port = oidc_port(&api)?;
    let clear = port.clear_cookies();
    match complete(&api, port.as_ref(), &parts, &query).await {
        Ok(response) => Ok(response),
        Err(error) => {
            api.record_best_effort(Entry {
                kind: AuditKind::LoginFailure,
                action: "auth.oidc_login",
                success: false,
                actor_name: "unknown",
                actor_id: None,
                client_ip: client_ip(&parts),
                detail: serde_json::json!({"code": error.code()}),
            })
            .await;
            Ok(failure(&parts, error, &clear))
        }
    }
}

async fn complete(
    api: &AuthApi,
    port: &dyn OidcLoginPort,
    parts: &Parts,
    query: &OidcCallbackQuery,
) -> Result<Response, ApiError> {
    let sessions = sessions_or_unavailable(api)?;
    let Some(authenticator) = api.app.authenticator() else {
        return Err(ApiError::unavailable("authentication_unavailable")
            .with_source("no authenticator is wired".into()));
    };
    let cookie_header = cookie_header_text(&parts.headers);
    let mut callback = OidcCallback::new(
        query.code.as_deref(),
        query.state.as_deref(),
        &cookie_header,
    );
    if let Some(error) = query.error.as_deref() {
        // Keycloak refused the login (the user said no, or the client is misconfigured). The
        // description is not shown to anyone: it is the provider's text.
        callback = callback.with_error(error);
    }
    let completion = port.complete(&callback).await.map_err(callback_error)?;
    let login = completion.login;
    let start = sessions.start(
        login.user_id,
        Some(login.snapshot),
        login.sso_carrier.as_deref(),
    )?;

    // The identity is real; whether it may enter is the chain's decision (group mapping, the
    // default role, the account state). The freshly signed token goes through it exactly as the
    // next request will, so a refusal happens now and no cookie is ever handed out for it.
    let mut context = AuthContext::new();
    if let Some(ip) = client_ip(parts) {
        context = context.with_client_ip(ip);
    }
    if let Some(id) = parts.extensions.get::<RequestId>() {
        context = context.with_request_id(id.as_str());
    }
    let principal = match authenticator
        .authenticate_detailed(&Credentials::Bearer(start.token.clone()), &context)
        .await
    {
        Ok(Some(principal)) if principal.auth_method == AuthMethod::InternalJwt => principal,
        Ok(_) => return Err(ApiError::from(AuthError::InvalidCredentials)),
        Err(failure) => {
            record_attempt_for(api.app.metrics(), failure.method, &failure.error);
            // The chain reports "no mapped group and no default role" in the two ways its
            // history left it; both mean the user may not enter.
            return Err(match failure.error {
                AuthError::GroupLookupFailed(_) => ApiError::forbidden(),
                other => ApiError::from(other),
            });
        }
    };
    api.record(Entry {
        kind: AuditKind::LoginSuccess,
        action: "auth.oidc_login",
        success: true,
        actor_name: &principal.username,
        actor_id: principal.user_id,
        client_ip: client_ip(parts),
        detail: serde_json::json!({"method": "oidc"}),
    })
    .await?;

    let mut cookies = completion.clear_cookies;
    cookies.extend(start.cookies);
    redirect(&sanitize_return_to(Some(&completion.return_to)), &cookies)
}

#[cfg(test)]
mod tests;
