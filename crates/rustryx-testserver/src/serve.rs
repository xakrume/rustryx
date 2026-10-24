//! The HTTP layer of the harness: it does what `rustryx-api` and the artifact module of the
//! server do for `/repository/{name}/{*path}` and hands the request to the real
//! `FormatEngine`.
//!
//! Authorization is the engine's (classify, then `Rbac`); this file only authenticates,
//! builds the `RequestHead` and decides which `WWW-Authenticate` challenge a refusal carries.
//! The path below the repository is passed on RAW (still percent-encoded) because the
//! format's `classify` must decode it exactly once: an axum `{*path}` extractor would decode
//! it first and break the traversal checks.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::response::Response;
use http::header::{AUTHORIZATION, HOST, USER_AGENT};
use http::{HeaderMap, HeaderValue, StatusCode};
use rustryx_core::model::RepoFormat;
use rustryx_core::names::RepoName;
use rustryx_core::ports::RepositoryRepo;
use rustryx_format_core::{ClientInfo, FormatEngine, RequestHead};
use url::Url;

use crate::auth::{CredentialBook, Outcome};
use crate::http_util::{ensure_challenge, envelope_error, oci_error};
use crate::oci;

/// Header carrying the id of the request, for correlation with engine logs.
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Shared state of the routes.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) engine: FormatEngine,
    pub(crate) repositories: Arc<dyn RepositoryRepo>,
    pub(crate) auth: Arc<CredentialBook>,
    /// Used for the base URL of a request that carries no `Host` header (HTTP/1.0).
    pub(crate) fallback_origin: Url,
    pub(crate) counter: Arc<AtomicU64>,
}

/// The router: one fallback that routes by hand, because the raw path matters (see above).
pub(crate) fn router(state: AppState) -> Router {
    Router::new().fallback(dispatch).with_state(state)
}

async fn dispatch(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    let request_id = format!("test-{}", state.counter.fetch_add(1, Ordering::Relaxed) + 1);
    let mut response = if let Some(below) = path.strip_prefix("/repository/") {
        let (name, rest) = below.split_once('/').unwrap_or((below, ""));
        let target = Target {
            name,
            head_path: rest.to_owned(),
            oci_scope: None,
            oci_route: false,
        };
        serve_repository(&state, &target, peer, &request_id, request).await
    } else if path == "/v2" || path.starts_with("/v2/") {
        oci::serve(&state, &path, peer, &request_id, request).await
    } else {
        envelope_error(StatusCode::NOT_FOUND, "route_not_found", "no such route")
    };
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .entry(REQUEST_ID_HEADER)
            .or_insert(value);
    }
    response
}

/// Which repository a request addresses and how the engine will see its path.
pub(crate) struct Target<'a> {
    /// The repository name as written in the URL.
    pub(crate) name: &'a str,
    /// The path handed to the format, raw, without a leading slash.
    pub(crate) head_path: String,
    /// The OCI scope a 401 challenge asks for (`repository:<name>:pull`).
    pub(crate) oci_scope: Option<String>,
    /// True for the `/v2` routes: errors use the OCI shape and the challenge is a Bearer one.
    pub(crate) oci_route: bool,
}

/// The origin clients used to reach the node, from the `Host` header.
///
/// Documents written by formats (tarball URLs, archive URLs) must point back at the address
/// the client used: a container reaches the harness as `host.docker.internal`, the host as
/// `127.0.0.1`. A `Host` that is not a valid authority falls back to the bound address.
pub(crate) fn origin_of(headers: &HeaderMap, fallback: &Url) -> Url {
    headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|host| Url::parse(&format!("http://{host}")).ok())
        .unwrap_or_else(|| fallback.clone())
}

/// The challenge a 401 carries for a repository of `format`.
///
/// pub clients erase their stored token on 401 and keep it on 403, so pub answers
/// `Bearer realm="pub"` on both (README 8.5); registry clients follow a Bearer challenge to
/// the token endpoint; everything else is a Basic challenge like the real server's default.
pub(crate) fn challenge_for(format: RepoFormat, origin: &Url, scope: Option<&str>) -> String {
    match format {
        RepoFormat::Pub => r#"Bearer realm="pub""#.to_owned(),
        RepoFormat::Docker | RepoFormat::Oci => bearer_challenge(origin, scope),
        _ => r#"Basic realm="rustryx""#.to_owned(),
    }
}

/// `Bearer realm="<origin>/v2/token",service="rustryx"[,scope="..."]`.
pub(crate) fn bearer_challenge(origin: &Url, scope: Option<&str>) -> String {
    let base = origin.as_str().trim_end_matches('/');
    let mut challenge = format!(r#"Bearer realm="{base}/v2/token",service="rustryx""#);
    if let Some(scope) = scope {
        let _ = write!(challenge, r#",scope="{scope}""#);
    }
    challenge
}

/// Serves one request on a repository: look it up, authenticate, run the engine.
pub(crate) async fn serve_repository(
    state: &AppState,
    target: &Target<'_>,
    peer: SocketAddr,
    request_id: &str,
    request: Request,
) -> Response {
    let not_found = |what: &str| {
        if target.oci_route {
            oci_error(StatusCode::NOT_FOUND, "NAME_UNKNOWN", what)
        } else {
            envelope_error(StatusCode::NOT_FOUND, "repository_not_found", what)
        }
    };
    // An invalid name cannot exist: the same answer as an unknown one, without a lookup.
    let Ok(name) = RepoName::parse(target.name) else {
        return not_found("no such repository");
    };
    let repository = match state.repositories.get_by_name(&name).await {
        Ok(Some(repository)) => repository,
        Ok(None) => return not_found("no such repository"),
        Err(error) => {
            tracing::warn!(%error, "repository lookup failed in the harness");
            return unavailable(target.oci_route);
        }
    };
    let (parts, body) = request.into_parts();
    let origin = origin_of(&parts.headers, &state.fallback_origin);
    let challenge = challenge_for(repository.format, &origin, target.oci_scope.as_deref());
    let principal = match state.auth.authenticate(&parts.headers).await {
        Outcome::Anonymous => rustryx_core::identity::Principal::anonymous(),
        Outcome::Principal(principal) => *principal,
        Outcome::Rejected => return unauthorized(target.oci_route, &challenge),
        Outcome::Unavailable => return unavailable(target.oci_route),
    };
    let user_agent = parts
        .headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut headers = parts.headers;
    if let Some(value) = headers.get_mut(AUTHORIZATION) {
        // So that a `Debug` of the head or a log line never shows a credential.
        value.set_sensitive(true);
    }
    let mut head = RequestHead::new(parts.method, target.head_path.clone(), principal, origin)
        .with_headers(headers)
        .with_client(ClientInfo {
            ip: Some(peer.ip()),
            user_agent,
            request_id: Some(request_id.to_owned()),
            download_source: None,
        });
    if let Some(query) = parts.uri.query() {
        head = head.with_query(query);
    }
    let format = repository.format;
    let mut response = state.engine.serve(Arc::new(repository), head, body).await;
    let refused = response.status() == StatusCode::UNAUTHORIZED
        || (format == RepoFormat::Pub && response.status() == StatusCode::FORBIDDEN);
    if refused {
        ensure_challenge(&mut response, &challenge);
    }
    response
}

fn unauthorized(oci_route: bool, challenge: &str) -> Response {
    let mut response = if oci_route {
        oci_error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "authentication required",
        )
    } else {
        envelope_error(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "the credentials were not accepted",
        )
    };
    ensure_challenge(&mut response, challenge);
    response
}

fn unavailable(oci_route: bool) -> Response {
    let mut response = if oci_route {
        oci_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "UNAVAILABLE",
            "a dependency is unavailable",
        )
    } else {
        envelope_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "dependency_unavailable",
            "a dependency is unavailable",
        )
    };
    response
        .headers_mut()
        .insert(http::header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    #[test]
    fn origin_follows_the_host_header_and_falls_back() {
        let fallback = Url::parse("http://127.0.0.1:9").unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(origin_of(&headers, &fallback), fallback);
        headers.insert(HOST, HeaderValue::from_static("host.docker.internal:9"));
        assert_eq!(
            origin_of(&headers, &fallback).as_str(),
            "http://host.docker.internal:9/"
        );
        headers.insert(HOST, HeaderValue::from_static("localhost:9"));
        assert_eq!(
            origin_of(&headers, &fallback).as_str(),
            "http://localhost:9/"
        );
        headers.insert(HOST, HeaderValue::from_static("not a host"));
        assert_eq!(origin_of(&headers, &fallback), fallback);
    }
}
