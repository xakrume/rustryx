//! Router assembly and the extension point for route modules.
//!
//! Three groups, each with its own layers:
//!
//! | group | routes | layers (outer to inner) |
//! |---|---|---|
//! | ops | `/healthz`, `/readyz`, `/metrics` | none of the limiting layers, no authentication |
//! | api | metadata and management routes | load shed, timeout, body limit, CORS, compression, no-store, authentication |
//! | artifacts | package format routes | load shed, timeout, large body limit and idle timeouts, authentication |
//!
//! Over all three, outermost first: request id, request span and metrics, static
//! security headers, panic recovery. Compression is not applied to artifacts:
//! packages are already compressed, and compressing a stream costs CPU on the
//! hot path while breaking `Content-Length` and range requests.
//!
//! The concurrency limit is one semaphore shared by api and artifacts (not one per
//! route; `Router::layer` clones a layer into every route, and a plain
//! `ConcurrencyLimitLayer` would then limit each route separately).

use std::sync::Arc;

use axum::Router;
use axum::error_handling::HandleErrorLayer;
use axum::middleware::{from_fn, from_fn_with_state};
use tokio::sync::Semaphore;
use tower::ServiceBuilder;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower::load_shed::LoadShedLayer;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;

use crate::auth::authenticate;
use crate::health;
use crate::layers::{
    BodyLimits, body_limits, cors, method_not_allowed, no_store_by_default, not_found, observe,
    panic_response, request_timeout, security_headers, shed_response,
};
use crate::rate_limit::{LimitState, RateLimiter, rate_limit};
use crate::request_id::request_id_layer;
use crate::state::AppState;

/// A feature that contributes routes without editing this crate.
///
/// The composition root creates one module per feature (`/service/rest/v1`,
/// `/repository/{name}/...`, `/v2/...`) and passes them to
/// [`RouterBuilder::module`]. A module owns its own state: it can call
/// `Router::with_state(its_state)` on a sub-router, which yields a
/// `Router<AppState>` ready to merge, so no field has to be added to [`AppState`].
pub trait RouteModule: Send + Sync {
    /// Name used in logs when the router is assembled.
    fn name(&self) -> &'static str;

    /// Adds routes to the API group (JSON metadata and management endpoints):
    /// compressed, no-store by default, CORS applies, small body limit.
    fn mount_api(&self, router: Router<AppState>) -> Router<AppState> {
        router
    }

    /// Adds routes to the artifacts group (package protocols and downloads): not
    /// compressed, large body limit, idle timeouts on both bodies.
    fn mount_artifacts(&self, router: Router<AppState>) -> Router<AppState> {
        router
    }
}

/// Builds the router from shared state and route modules.
pub struct RouterBuilder {
    state: AppState,
    modules: Vec<Arc<dyn RouteModule>>,
}

impl RouterBuilder {
    /// Starts a builder.
    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            modules: Vec::new(),
        }
    }

    /// Adds a route module. Modules are mounted in the order they are added.
    #[must_use]
    pub fn module(mut self, module: Arc<dyn RouteModule>) -> Self {
        self.modules.push(module);
        self
    }

    /// Assembles the router.
    pub fn build(self) -> Router {
        let Self { state, modules } = self;
        let mut api = Router::new();
        let mut artifacts = Router::new();
        for module in &modules {
            tracing::debug!(module = module.name(), "mounting route module");
            api = module.mount_api(api);
            artifacts = module.mount_artifacts(artifacts);
        }
        let in_flight = Arc::new(Semaphore::new(state.settings().max_in_flight));
        // One limiter for both groups: a client's budget is not doubled by spreading
        // requests over metadata and artifact routes.
        let limit = state.settings().rate_limit.map(|settings| LimitState {
            limiter: Arc::new(RateLimiter::new(
                settings.requests_per_second,
                settings.burst,
            )),
            metrics: state.metrics().cloned(),
        });

        // The 405 handler is set per group, before the group's layers are applied, so
        // that those layers wrap it: a CORS preflight (an `OPTIONS` request that no
        // route declares) must reach the CORS layer instead of a bare 405, and
        // setting it later would replace the wrapped handler.
        let ops = health::routes()
            .method_not_allowed_fallback(method_not_allowed)
            .with_state(state.clone());
        let api = api_group(api, &state, &in_flight, limit.as_ref());
        let artifacts = artifact_group(artifacts, &state, &in_flight, limit.as_ref());

        let router = Router::new()
            .merge(ops)
            .merge(api)
            .merge(artifacts)
            .fallback(not_found);
        // Applied last so that they also wrap the fallbacks: a 404 for an unknown
        // path still gets a request id, security headers and a metrics sample.
        router
            .layer(CatchPanicLayer::custom(panic_response))
            .layer(from_fn(security_headers))
            .layer(from_fn_with_state(state, observe))
            .layer(from_fn(request_id_layer))
    }
}

/// Builds the router with no extra modules (probes, metrics, envelope, fallbacks).
pub fn build_router(state: AppState) -> Router {
    RouterBuilder::new(state).build()
}

fn api_group(
    routes: Router<AppState>,
    state: &AppState,
    in_flight: &Arc<Semaphore>,
    limit: Option<&LimitState>,
) -> Router {
    let settings = state.settings();
    let limits = BodyLimits {
        max_bytes: settings.max_body_bytes,
        idle: settings.body_idle_timeout,
        response_idle: false,
    };
    let routes = with_rate_limit(
        routes.method_not_allowed_fallback(method_not_allowed),
        limit,
    )
    .layer(from_fn_with_state(state.clone(), authenticate))
    .layer(from_fn(no_store_by_default))
    .layer(CompressionLayer::new());
    // CORS sits outside authentication so that a browser can read a 401, and a
    // preflight (which carries no credentials) is answered before anything else.
    let routes = match cors(settings) {
        Some(cors) => routes.layer(cors),
        None => routes,
    };
    let routes = routes
        .layer(from_fn_with_state(limits, body_limits))
        .layer(from_fn_with_state(state.clone(), request_timeout));
    shed(routes, state, in_flight).with_state(state.clone())
}

fn artifact_group(
    routes: Router<AppState>,
    state: &AppState,
    in_flight: &Arc<Semaphore>,
    limit: Option<&LimitState>,
) -> Router {
    let settings = state.settings();
    let limits = BodyLimits {
        max_bytes: settings.max_blob_bytes,
        idle: settings.body_idle_timeout,
        response_idle: true,
    };
    let routes = with_rate_limit(
        routes.method_not_allowed_fallback(method_not_allowed),
        limit,
    )
    .layer(from_fn_with_state(state.clone(), authenticate))
    .layer(from_fn_with_state(limits, body_limits))
    .layer(from_fn_with_state(state.clone(), request_timeout));
    shed(routes, state, in_flight).with_state(state.clone())
}

/// Innermost layer of a group, directly inside authentication so the limiter can key
/// on the authenticated user.
fn with_rate_limit(routes: Router<AppState>, limit: Option<&LimitState>) -> Router<AppState> {
    match limit {
        Some(state) => routes.layer(from_fn_with_state(state.clone(), rate_limit)),
        None => routes,
    }
}

/// Outermost layer of a group: bounded concurrency, and shedding beyond the bound.
///
/// One `ServiceBuilder` on purpose: `Router::layer` only accepts layers whose
/// service cannot fail, and `LoadShed` fails with `Overloaded` until the
/// `HandleErrorLayer` above it turns that into a response.
fn shed(
    routes: Router<AppState>,
    state: &AppState,
    in_flight: &Arc<Semaphore>,
) -> Router<AppState> {
    let metrics = state.metrics().cloned();
    let retry_after = state.settings().shed_retry_after;
    routes.layer(
        ServiceBuilder::new()
            .layer(HandleErrorLayer::new(move |error: tower::BoxError| {
                let metrics = metrics.clone();
                async move { shed_response(&error, metrics.as_ref(), retry_after) }
            }))
            .layer(LoadShedLayer::new())
            .layer(GlobalConcurrencyLimitLayer::with_semaphore(
                in_flight.clone(),
            )),
    )
}

#[cfg(test)]
mod tests;
