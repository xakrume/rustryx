//! `/repository/{name}/...` and `/v2/...`: the routes that hand artifact traffic to the format
//! engine.
//!
//! Authorization is the engine's: it classifies the request first and asks the authorizer about
//! the repository, the action and the coordinate, so a handler here must not decide anything.
//! That is why the routes are declared as delegated (`RouteSet::delegated`, the one spelled out
//! opt-out of the structural deny-by-default) and why this file stays free of any permission check:
//! all it does is find the repository, build the request head and copy the engine's labels onto
//! the response.
//!
//! Format handlers are registered in `wiring::formats`. `/v2` is the OCI distribution API:
//! the ping, the token endpoint and the catalog are mounted here, and
//! `/v2/<repository>/...` is the same engine call as `/repository/<repository>/v2/...`.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use http::header::{AUTHORIZATION, USER_AGENT};
use http::{HeaderMap, StatusCode};
use rustryx_api::{
    ApiError, RequestClientIp, RequestPrincipal, RouteModule, RouteSet, current_request_id,
    record_repo,
};
use rustryx_core::identity::Principal;
use rustryx_core::names::RepoName;
use rustryx_core::ports::{Authenticator, RepositoryRepo};
use rustryx_format_core::{
    CacheLabel, ClientInfo, FormatEngine, RequestClass as FormatClass, RequestHead,
    RequestLabels as FormatLabels,
};
use rustryx_format_oci::{CatalogService, OciTokenService};
use rustryx_observability::{CacheStatus, RequestClass, RequestLabels};
use url::Url;

/// Why the delegated routes are exempt from the up-front requirement, as it appears in the
/// route catalogue.
const REASON: &str = "format engine: it classifies the request and authorizes it per repository, action and coordinate";

mod oci;

#[derive(Clone)]
struct ArtifactState {
    engine: FormatEngine,
    repositories: Arc<dyn RepositoryRepo>,
    base_url: Option<Url>,
    /// Scheme of the fallback base URL when none is configured (development).
    fallback_scheme: &'static str,
    /// Signs and checks `/v2/token` bearers.
    tokens: OciTokenService,
    /// `GET /v2/_catalog`.
    catalog: CatalogService,
    /// Password grant of `POST /v2/token`. `None` in a process that has no authenticator.
    authenticator: Option<Arc<dyn Authenticator>>,
}

/// The module that mounts the artifact routes.
pub struct ArtifactModule {
    state: ArtifactState,
}

impl ArtifactModule {
    /// A module serving through `engine`. `base_url` is the public URL formats write into
    /// documents (`server.base_url`); without it the request's `Host` is used, over `http` when
    /// `require_https` is off and `https` otherwise.
    #[must_use]
    pub fn new(
        engine: FormatEngine,
        repositories: Arc<dyn RepositoryRepo>,
        base_url: Option<Url>,
        require_https: bool,
        tokens: OciTokenService,
        catalog: CatalogService,
        authenticator: Option<Arc<dyn Authenticator>>,
    ) -> Self {
        Self {
            state: ArtifactState {
                engine,
                repositories,
                base_url,
                fallback_scheme: if require_https { "https" } else { "http" },
                tokens,
                catalog,
                authenticator,
            },
        }
    }
}

impl RouteModule for ArtifactModule {
    fn name(&self) -> &'static str {
        "artifacts"
    }

    fn mount_artifacts(&self, routes: RouteSet) -> RouteSet {
        let artifact_routes = Router::new()
            .route("/repository/{name}", any(serve_repository))
            .route("/repository/{name}/", any(serve_repository))
            .route("/repository/{name}/{*path}", any(serve_repository))
            .route("/v2", get(oci_ping))
            .route("/v2/", get(oci_ping))
            .route("/v2/token", get(oci_token).post(token_post))
            .route("/v2/_catalog", get(oci_catalog))
            .route("/v2/{*path}", any(oci_repository))
            .with_state(self.state.clone());
        routes.delegated(REASON, artifact_routes)
    }
}

/// Splits `/repository/<name>[/<rest>]` into the name and the raw rest, which stays
/// percent-encoded and loses its leading slash: the format decodes it exactly once.
fn split_path(path: &str) -> Option<(&str, &str)> {
    let below = path.strip_prefix("/repository/")?;
    Some(below.split_once('/').unwrap_or((below, "")))
}

fn not_found() -> Response {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "repository_not_found",
        "No such repository",
    )
    .into_response()
}

/// The labels the engine decided, in the vocabulary of the metrics crate.
fn metric_labels(labels: FormatLabels) -> RequestLabels {
    let class = match labels.class {
        FormatClass::Metadata => RequestClass::Metadata,
        FormatClass::Download => RequestClass::Download,
        FormatClass::Upload => RequestClass::Upload,
        FormatClass::Api => RequestClass::Api,
        FormatClass::Other => RequestClass::Other,
    };
    let cache = match labels.cache {
        CacheLabel::Hit => CacheStatus::Hit,
        CacheLabel::Miss => CacheStatus::Miss,
        CacheLabel::Stale => CacheStatus::Stale,
        CacheLabel::Bypass => CacheStatus::Bypass,
        CacheLabel::None => CacheStatus::Uncached,
    };
    RequestLabels::class(class).with_cache(cache)
}

/// The URL clients use to reach this node. A `Host` that is not a valid authority is a client
/// error; it is never echoed.
fn base_url(state: &ArtifactState, headers: &HeaderMap) -> Result<Url, ApiError> {
    if let Some(url) = &state.base_url {
        return Ok(url.clone());
    }
    let host = headers
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    Url::parse(&format!("{}://{host}", state.fallback_scheme))
        .map_err(|_| ApiError::bad_request("invalid_host", "The Host header is not valid"))
}

fn sensitive_authorization(mut headers: HeaderMap) -> HeaderMap {
    if let Some(value) = headers.get_mut(AUTHORIZATION) {
        value.set_sensitive(true);
    }
    headers
}

async fn serve_repository(
    State(state): State<ArtifactState>,
    RequestPrincipal(principal): RequestPrincipal,
    RequestClientIp(ip): RequestClientIp,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let Some((raw_name, rest)) = split_path(parts.uri.path()) else {
        return not_found();
    };
    // An invalid name cannot exist: the same answer as an unknown one, without a lookup.
    let Ok(name) = RepoName::parse(raw_name) else {
        return not_found();
    };
    record_repo(raw_name);
    let repository = match state.repositories.get_by_name(&name).await {
        Ok(Some(repository)) => repository,
        Ok(None) => return not_found(),
        Err(error) => return ApiError::from(error).into_response(),
    };
    let base = match base_url(&state, &parts.headers) {
        Ok(url) => url,
        Err(error) => return error.into_response(),
    };
    let user_agent = parts
        .headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut head = RequestHead::new(parts.method.clone(), rest, principal, base)
        .with_headers(sensitive_authorization(parts.headers))
        .with_client(ClientInfo {
            ip,
            user_agent,
            request_id: current_request_id().map(|id| id.as_str().to_owned()),
            ..ClientInfo::default()
        });
    if let Some(query) = parts.uri.query() {
        head = head.with_query(query);
    }
    let mut response = state.engine.serve(Arc::new(repository), head, body).await;
    let labels = metric_labels(FormatLabels::of(&response));
    response.extensions_mut().insert(labels);
    response
}

async fn oci_ping(
    State(state): State<ArtifactState>,
    RequestPrincipal(principal): RequestPrincipal,
    request: Request,
) -> Response {
    oci::ping(&state, request.headers(), &principal)
}

async fn oci_token(
    State(state): State<ArtifactState>,
    RequestPrincipal(principal): RequestPrincipal,
    request: Request,
) -> Response {
    oci::token(&state, request, principal).await
}

async fn token_post(State(state): State<ArtifactState>, request: Request) -> Response {
    oci::token(&state, request, Principal::anonymous()).await
}

async fn oci_catalog(
    State(state): State<ArtifactState>,
    RequestPrincipal(principal): RequestPrincipal,
    request: Request,
) -> Response {
    oci::catalog(&state, request.headers(), &principal, request.uri().query()).await
}

async fn oci_repository(
    State(state): State<ArtifactState>,
    RequestPrincipal(principal): RequestPrincipal,
    RequestClientIp(ip): RequestClientIp,
    request: Request,
) -> Response {
    oci::repository(&state, request, principal, ip).await
}

#[cfg(test)]
mod tests;
