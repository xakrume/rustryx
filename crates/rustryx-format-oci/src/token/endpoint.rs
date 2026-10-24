//! The HTTP side of the token service: the `/v2/token` answer, the `/v2/` ping and the
//! `WWW-Authenticate` challenge. The API crate mounts these; it authenticates the caller
//! (Basic or none) and passes the principal in.

use std::fmt::Write as _;

use axum_core::body::Body;
use http::{HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::identity::Principal;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use url::Url;

use super::service::{OciTokenService, TokenRequest, TokenRequestError};
use crate::error::{OciError, OciErrorCode};
use crate::headers::{json_response, stamp_version};

/// The `WWW-Authenticate` value that sends a client to the token endpoint:
/// `Bearer realm="<base>/v2/token",service="<service>"[,scope="<scope>"][,error="<error>"]`.
///
/// `scope` is the scope the failed request needs (see [`challenge_scope`]) so the client asks
/// for it at once; `error` is `insufficient_scope` when the client had a token that did not
/// carry it.
#[must_use]
pub fn bearer_challenge(
    base_url: &Url,
    service: &str,
    scope: Option<&str>,
    error: Option<&str>,
) -> String {
    let base = base_url.as_str().trim_end_matches('/');
    let mut challenge = format!(
        "Bearer realm=\"{base}/v2/token\",service=\"{}\"",
        quoted(service)
    );
    for (key, value) in [("scope", scope), ("error", error)] {
        if let Some(value) = value {
            let _ = write!(challenge, ",{key}=\"{}\"", quoted(value));
        }
    }
    challenge
}

/// The scope a request needs: `repository:<repo>/<image>:pull` for reads and
/// `pull,push` for writes (a push also reads, to check what exists), `pull,delete` for a
/// delete.
#[must_use]
pub fn challenge_scope(repository: &str, image: &str, method: &Method) -> String {
    let actions = if method == Method::GET || method == Method::HEAD {
        "pull"
    } else if method == Method::DELETE {
        "pull,delete"
    } else {
        "pull,push"
    };
    format!("repository:{repository}/{image}:{actions}")
}

/// `GET /v2/` without a valid token: 401 with the bearer challenge, always, also when the
/// repository would allow anonymous pull. The client then fetches a token (an anonymous one
/// when it has no credentials) and `docker login` can verify credentials.
#[must_use]
pub fn ping_challenge(base_url: &Url, service: &str) -> Response<Body> {
    let mut response = OciError::new(
        StatusCode::UNAUTHORIZED,
        OciErrorCode::Unauthorized,
        "authentication required",
    )
    .into_response();
    if let Ok(value) = HeaderValue::from_str(&bearer_challenge(base_url, service, None, None)) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// `GET /v2/` with a valid token or credentials: 200 and an empty object.
#[must_use]
pub fn ping_ok() -> Response<Body> {
    json_response(StatusCode::OK, "application/json", &json!({}))
}

/// Answers a token request for an authenticated (or anonymous) principal.
///
/// 200 with `{"token","access_token","expires_in","issued_at"}` (and `scope` for the `OAuth2`
/// `POST`, which says what was granted); 400 for a malformed scope or an unknown service;
/// 503 with `Retry-After` when authorization is unavailable. A principal without access to
/// what it asked for still gets a 200 and a token without it, as the specification says.
pub async fn token_response(
    service: &OciTokenService,
    principal: &Principal,
    request: &TokenRequest,
    oauth: bool,
) -> Response<Body> {
    match service.issue(principal, request).await {
        Ok(issued) => {
            let issued_at = issued.issued_at.format(&Rfc3339).unwrap_or_default();
            let mut body = json!({
                "token": issued.token,
                "access_token": issued.token,
                "expires_in": issued.expires_in,
                "issued_at": issued_at,
            });
            if oauth {
                body["scope"] = serde_json::Value::String(issued.granted_scope);
            }
            let mut response = json_response(StatusCode::OK, "application/json", &body);
            // A token is a credential: no shared cache may keep it.
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(error) => refusal(&error),
    }
}

fn refusal(error: &TokenRequestError) -> Response<Body> {
    let mut response = match error {
        TokenRequestError::UnknownService => {
            OciError::unsupported(StatusCode::BAD_REQUEST, "unknown service")
        }
        TokenRequestError::InvalidScope(_) => {
            OciError::unsupported(StatusCode::BAD_REQUEST, "invalid scope")
        }
        TokenRequestError::Unavailable => OciError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            OciErrorCode::Unavailable,
            "authorization is temporarily unavailable",
        )
        .with_header(header::RETRY_AFTER, HeaderValue::from_static("1")),
        TokenRequestError::Signing(_) => OciError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            OciErrorCode::Unknown,
            "token could not be issued",
        ),
    }
    .into_response();
    stamp_version(response.headers_mut());
    response
}

/// Escapes the two characters that could end a quoted challenge parameter early. The
/// inputs are service names and validated scopes, so this is a second line of defence.
fn quoted(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests;
