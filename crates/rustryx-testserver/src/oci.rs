//! The OCI distribution entry points the harness owns: the `/v2/` ping and the `/v2/token`
//! endpoint, plus the mapping of `/v2/<repository>/<image>/...` onto a repository.
//!
//! Path mapping: the first segment below `/v2/` is the Rustryx repository (a `RepoName` is
//! valid as a docker repository prefix), the rest is the image path. The format sees the SAME
//! `head.path` whichever door the client used: `v2/<image>/manifests/<ref>`, exactly what
//! `/repository/<repository>/v2/<image>/manifests/<ref>` gives it.

use std::net::SocketAddr;

use axum::extract::Request;
use axum::response::Response;
use http::header::{CONTENT_TYPE, WWW_AUTHENTICATE};
use http::{HeaderMap, Method, StatusCode};
use rustryx_core::identity::Principal;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::auth::Outcome;
use crate::http_util::{DISTRIBUTION_VERSION_HEADER, ensure_challenge, json_response, oci_error};
use crate::serve::{AppState, Target, bearer_challenge, origin_of, serve_repository};

/// Largest form body the token endpoint reads (an `OAuth2` password grant is a few hundred bytes).
const MAX_TOKEN_FORM_BYTES: usize = 64 * 1024;

/// Handles everything below `/v2`.
pub(crate) async fn serve(
    state: &AppState,
    path: &str,
    peer: SocketAddr,
    request_id: &str,
    request: Request,
) -> Response {
    let rest = path.strip_prefix("/v2").unwrap_or("");
    match rest {
        "" | "/" => ping(state, request.headers()).await,
        "/token" => token(state, request).await,
        other => {
            let below = other.strip_prefix('/').unwrap_or(other);
            let (name, image_path) = below.split_once('/').unwrap_or((below, ""));
            let target = Target {
                name,
                head_path: format!("v2/{image_path}"),
                oci_scope: image_scope(name, image_path, request.method()),
                oci_route: true,
            };
            serve_repository(state, &target, peer, request_id, request).await
        }
    }
}

/// The scope a challenge asks for: `repository:<name>/<image>:pull`, `pull,push` on a
/// write, or `pull,delete` on a delete. Same actions as the format's `challenge_scope`.
///
/// The image is whatever precedes the protocol marker (`manifests`, `blobs`, `tags`,
/// `referrers`). A path without a marker has no scope to ask for.
pub(crate) fn image_scope(repository: &str, image_path: &str, method: &Method) -> Option<String> {
    let segments: Vec<&str> = image_path.split('/').collect();
    let marker = segments
        .iter()
        .position(|s| matches!(*s, "manifests" | "blobs" | "tags" | "referrers"))?;
    if marker == 0 {
        return None;
    }
    let image = segments[..marker].join("/");
    let actions = if method == Method::GET || method == Method::HEAD {
        "pull"
    } else if method == Method::DELETE {
        "pull,delete"
    } else {
        "pull,push"
    };
    Some(format!("repository:{repository}/{image}:{actions}"))
}

/// `GET /v2/`: 401 with a Bearer challenge unless the request carries a credential the
/// harness accepts.
///
/// A client without a credential ALWAYS gets the challenge, also when anonymous pull is on:
/// it then fetches an anonymous token from the token endpoint, which is the flow
/// `docker pull` expects. A valid registry token answers 200, because `docker login`
/// verifies the token it was just given with a `GET /v2/` and fails on anything but 200.
async fn ping(state: &AppState, headers: &HeaderMap) -> Response {
    let origin = origin_of(headers, &state.fallback_origin);
    let outcome = state.auth.authenticate(headers).await;
    let mut response = match outcome {
        Outcome::Principal(_) => json_response(StatusCode::OK, &serde_json::json!({})),
        Outcome::Anonymous | Outcome::Rejected => {
            let mut response = oci_error(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "authentication required",
            );
            ensure_challenge(&mut response, &bearer_challenge(&origin, None));
            response
        }
        Outcome::Unavailable => dependency_unavailable(),
    };
    stamp_version(&mut response);
    response
}

fn stamp_version(response: &mut Response) {
    response.headers_mut().insert(
        DISTRIBUTION_VERSION_HEADER,
        http::HeaderValue::from_static("registry/2.0"),
    );
}

/// 503 when the store that holds roles cannot be read. `Retry-After` is set so a client
/// does not treat the failure as "no roles" or as a permanent refusal.
fn dependency_unavailable() -> Response {
    let mut response = oci_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "UNAVAILABLE",
        "a dependency is unavailable",
    );
    response.headers_mut().insert(
        http::header::RETRY_AFTER,
        http::HeaderValue::from_static("1"),
    );
    response
}

/// `/v2/token`: `GET` with Basic credentials (`docker login`, skopeo, helm) or none (an
/// anonymous token), and the `OAuth2` `POST` with `grant_type=password`.
///
/// Anonymous tokens are always issued. Whether they are worth anything is `Rbac`'s call (the
/// global switch and the repository flag), exactly as for a request without a token.
async fn token(state: &AppState, request: Request) -> Response {
    let post = request.method() == Method::POST;
    if !post && request.method() != Method::GET {
        return oci_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "UNSUPPORTED",
            "the token endpoint answers GET and POST",
        );
    }
    let (parts, body) = request.into_parts();
    let query: Vec<(String, String)> = parts
        .uri
        .query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    if post {
        let Ok(bytes) = axum::body::to_bytes(body, MAX_TOKEN_FORM_BYTES).await else {
            return oci_error(
                StatusCode::BAD_REQUEST,
                "UNSUPPORTED",
                "unreadable token request",
            );
        };
        let form: Vec<(String, String)> =
            url::form_urlencoded::parse(&bytes).into_owned().collect();
        return password_grant(state, &form).await;
    }
    let outcome = state.auth.authenticate(&parts.headers).await;
    let scope = first(&query, "scope");
    match outcome {
        Outcome::Anonymous => issue(state, &Principal::anonymous(), scope, false),
        Outcome::Principal(principal) => issue(state, &principal, scope, false),
        Outcome::Rejected => token_refused(),
        Outcome::Unavailable => dependency_unavailable(),
    }
}

async fn password_grant(state: &AppState, form: &[(String, String)]) -> Response {
    if first(form, "grant_type") != Some("password") {
        return json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "unsupported_grant_type" }),
        );
    }
    let username = first(form, "username").unwrap_or_default();
    let password = first(form, "password").unwrap_or_default();
    match state.auth.basic(username, password).await {
        Outcome::Principal(principal) => issue(state, &principal, first(form, "scope"), true),
        Outcome::Unavailable => dependency_unavailable(),
        Outcome::Anonymous | Outcome::Rejected => token_refused(),
    }
}

fn first<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn token_refused() -> Response {
    let mut response = oci_error(
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "incorrect username or password",
    );
    ensure_challenge(&mut response, r#"Basic realm="rustryx token service""#);
    response
}

/// The token response. The Docker flow reads `token`, `OAuth2` clients read `access_token`;
/// both are the same string.
fn issue(state: &AppState, principal: &Principal, scope: Option<&str>, oauth: bool) -> Response {
    let (token, expires_in) = state.auth.issue_oci_token(principal);
    let issued_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    let mut body = serde_json::json!({
        "token": token,
        "access_token": token,
        "expires_in": expires_in,
        "issued_at": issued_at,
    });
    if oauth && let Some(scope) = scope {
        body["scope"] = serde_json::Value::String(scope.to_owned());
    }
    let mut response = json_response(StatusCode::OK, &body);
    response.headers_mut().insert(
        CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().remove(WWW_AUTHENTICATE);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_the_image_before_the_protocol_marker() {
        let get = Method::GET;
        assert_eq!(
            image_scope("docker-hosted", "team/app/manifests/1.0", &get).as_deref(),
            Some("repository:docker-hosted/team/app:pull")
        );
        assert_eq!(
            image_scope("r", "app/blobs/uploads/", &Method::POST).as_deref(),
            Some("repository:r/app:pull,push")
        );
        assert_eq!(
            image_scope("r", "app/manifests/1", &Method::DELETE).as_deref(),
            Some("repository:r/app:pull,delete")
        );
        assert_eq!(image_scope("r", "manifests/x", &get), None);
        assert_eq!(image_scope("r", "", &get), None);
        assert_eq!(image_scope("r", "app/nothing", &get), None);
    }
}
