//! Route declarations: every route says what it needs, and the router checks.
//!
//! A [`RouteModule`](crate::RouteModule) does not receive a raw `axum::Router` but a
//! [`RouteSet`]. The only way to add a route is [`RouteSet::route`], which takes a
//! [`Requires`], or [`RouteSet::delegated`], which names the reason a module authorizes
//! inside its own engine. There is no route without a declaration, so the catalogue
//! ([`RouteSet::declared`], [`RouteCatalog`]) is complete by construction, and a test can walk
//! it: every declared route that is not `Public` must refuse an anonymous caller.
//!
//! What the layer of a declared route does, in order:
//!
//! 1. `Authenticated`, `Interactive`, `Admin` and `App`: ask now, answer 401, 403 or 503
//!    without calling the handler, and hand the proof to the handler through the request
//!    extensions (the [`Authorized`](crate::Authorized) extractor reads it).
//! 2. `Repo` and `Project` need the resource, so the handler asks with
//!    `AppState::authorize` or `AppState::authorize_project`.
//! 3. Afterwards, for every requirement that is not `Public` or `Delegated`: a 2xx or 3xx
//!    answer produced without any successful ask in this request is replaced by 500
//!    `authorization_not_enforced` (see `authz.rs`).

use axum::Router;
use axum::extract::{Request, State};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use rustryx_core::identity::Principal;

use crate::auth::unauthenticated_for;
use crate::authz::{Requires, traced};
use crate::error::ApiError;
use crate::state::AppState;

/// One declared route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecl {
    /// The path template as given to [`RouteSet::route`] (for a delegated router: the reason).
    pub path: String,
    /// What the route needs.
    pub requires: Requires,
}

/// Every route a router serves with its requirement, per group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteCatalog {
    /// Routes of the API group.
    pub api: Vec<RouteDecl>,
    /// Routes of the artifacts group.
    pub artifacts: Vec<RouteDecl>,
}

impl RouteCatalog {
    /// Every declaration of both groups.
    pub fn all(&self) -> impl Iterator<Item = &RouteDecl> {
        self.api.iter().chain(self.artifacts.iter())
    }
}

/// The routes of one group while modules add to them.
pub struct RouteSet {
    router: Router<AppState>,
    state: AppState,
    declared: Vec<RouteDecl>,
}

impl RouteSet {
    pub(crate) fn new(state: AppState) -> Self {
        Self {
            router: Router::new(),
            state,
            declared: Vec::new(),
        }
    }

    /// The shared state, for a module that builds its own state from it
    /// (`AppState` is cheap to clone).
    #[must_use]
    pub fn app_state(&self) -> &AppState {
        &self.state
    }

    /// Adds a route. Every method of `routes` gets the same requirement; call this again with
    /// another requirement for the other methods of one path.
    #[must_use]
    pub fn route(mut self, path: &str, requires: Requires, routes: MethodRouter<AppState>) -> Self {
        let routes = if matches!(requires, Requires::Public | Requires::Delegated(_)) {
            routes
        } else {
            routes.route_layer(from_fn_with_state(
                Enforce {
                    requires,
                    state: self.state.clone(),
                },
                enforce,
            ))
        };
        self.router = self.router.route(path, routes);
        self.declared.push(RouteDecl {
            path: path.to_owned(),
            requires,
        });
        self
    }

    /// Merges a router whose handlers authorize inside their own engine (the format engine
    /// answers for `/repository/{name}/...` and `/v2/...`). `reason` says where, and lands in
    /// the catalogue. Deny by default then rests on that engine, which is why this is the one
    /// opt-out and is spelled out.
    #[must_use]
    pub fn delegated(mut self, reason: &'static str, router: Router<AppState>) -> Self {
        self.router = self.router.merge(router);
        self.declared.push(RouteDecl {
            path: reason.to_owned(),
            requires: Requires::Delegated(reason),
        });
        self
    }

    /// What was declared so far.
    #[must_use]
    pub fn declared(&self) -> &[RouteDecl] {
        &self.declared
    }

    pub(crate) fn into_parts(self) -> (Router<AppState>, Vec<RouteDecl>) {
        (self.router, self.declared)
    }
}

impl std::fmt::Debug for RouteSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteSet")
            .field("declared", &self.declared)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct Enforce {
    requires: Requires,
    state: AppState,
}

/// The layer of a declared, non-public route.
async fn enforce(State(enforce): State<Enforce>, mut request: Request, next: Next) -> Response {
    let Some(principal) = request.extensions().get::<Principal>().cloned() else {
        // Reaching a declared route without the authentication middleware is a wiring bug.
        return ApiError::internal(
            "no principal in request extensions: the authentication middleware is not applied to this route",
        )
        .into_response();
    };
    if let Err(error) = ask_up_front(&enforce, &principal, &mut request).await {
        // An anonymous caller whose session ended is told so, not just "authenticate".
        let error = if error.status() == http::StatusCode::UNAUTHORIZED
            && error.code() == "unauthenticated"
        {
            unauthenticated_for(request.extensions())
        } else {
            error
        };
        return error.into_response();
    }
    // Only these two leave the question to the handler; the others were answered above.
    let handler_asks = matches!(enforce.requires, Requires::Repo(_) | Requires::Project(_));
    let (response, asked) = traced(next.run(request)).await;
    let served = response.status().is_success() || response.status().is_redirection();
    if handler_asks && served && !asked {
        tracing::error!(
            requires = ?enforce.requires,
            "a handler answered without authorizing the request; the answer was withheld"
        );
        return ApiError::internal("handler served a request without authorizing it")
            .with_code("authorization_not_enforced")
            .into_response();
    }
    response
}

/// Requirements that need no resource are decided before the handler runs.
async fn ask_up_front(
    enforce: &Enforce,
    principal: &Principal,
    request: &mut Request,
) -> Result<(), ApiError> {
    let state = &enforce.state;
    match enforce.requires {
        Requires::Authenticated => {
            let proof = state.authorize_authenticated(principal)?;
            request.extensions_mut().insert(proof);
        }
        Requires::Interactive => {
            let proof = state.authorize_interactive(principal)?;
            request.extensions_mut().insert(proof);
        }
        Requires::Admin => {
            let proof = state.authorize_admin(principal).await?;
            request.extensions_mut().insert(proof);
        }
        Requires::App(area, action) => {
            let proof = state.authorize_app(principal, area, action).await?;
            request.extensions_mut().insert(proof);
        }
        _ => {}
    }
    Ok(())
}
