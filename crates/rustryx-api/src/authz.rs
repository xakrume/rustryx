//! Deny by default, made structural.
//!
//! An endpoint that never asks "may this caller do that" must not be able to serve anyone.
//! Three mechanisms hold that invariant, from the compiler outwards:
//!
//! 1. **The proof type.** [`Authorized<G>`] can only be produced by the `AppState::authorize*`
//!    methods of this module (its constructor is private to the module, see the
//!    `compile_fail` example below). A function that does the work, say
//!    `fn list_users(proof: &Authorized<AdminGrant>)`, cannot be called without having asked.
//! 2. **The route declaration.** A module adds routes through
//!    [`RouteSet::route`](crate::RouteSet::route) with a [`Requires`]. Requirements that need
//!    no resource (`Authenticated`, `Interactive`, `Admin`, `App`) are enforced by a layer in
//!    front of the handler and the proof is handed to it as the extractor [`Authorized`].
//! 3. **The backstop.** A route whose requirement is not `Public` and whose handler produced
//!    a 2xx or 3xx answer without any `authorize*` call having succeeded in that request is
//!    answered with 500 `authorization_not_enforced` and an error log instead: the handler
//!    forgot, and nothing it produced reaches the client. (Statuses of 4xx and 5xx pass: they
//!    serve no content. Existence leaks through a 404 are the handler's care, see
//!    `docs/api-conventions.md` section 12.)
//!
//! ```compile_fail,E0451
//! use rustryx_api::{AdminGrant, Authorized};
//! use rustryx_core::identity::Principal;
//!
//! // Outside this crate there is no way to build the proof by hand.
//! let forged = Authorized::<AdminGrant> { principal: Principal::anonymous(), grant: AdminGrant };
//! ```
//!
//! ```compile_fail,E0624
//! use rustryx_api::{AdminGrant, Authorized};
//!
//! // And the constructor is not public either.
//! let forged = Authorized::new(rustryx_core::identity::Principal::anonymous(), AdminGrant);
//! ```

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::FromRequestParts;
use http::request::Parts;
use rustryx_core::identity::{Action, AppArea, AuthMethod, Principal};
use rustryx_core::ids::ProjectId;
use rustryx_core::ports::{Decision, ResourceRef};
use rustryx_core::project::ProjectPermission;

use crate::error::ApiError;
use crate::state::AppState;

tokio::task_local! {
    /// Set by the first successful `authorize*` call of a request. Scoped around the handler by
    /// the route layer. A task-local because `AppState::authorize` has no request to hand it to.
    static AUTHORIZED: Arc<AtomicBool>;
}

/// Records that this request asked and was allowed. Outside a scope (a unit test that calls
/// `authorize` directly) it does nothing.
fn note_authorized() {
    // `try_with` fails only outside a scope, which is exactly the case this tolerates.
    let _ = AUTHORIZED.try_with(|flag| flag.store(true, Ordering::Release));
}

/// Runs `future` with a fresh trace and tells whether an `authorize*` call succeeded in it.
pub(crate) async fn traced<F: Future>(future: F) -> (F::Output, bool) {
    let flag = Arc::new(AtomicBool::new(false));
    let output = AUTHORIZED.scope(flag.clone(), future).await;
    (output, flag.load(Ordering::Acquire))
}

/// What a route needs from its caller (design 2.8). Declared once per route; the route layer and
/// the route catalogue read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Requires {
    /// Login, probes, anonymous reads gated by the repository.
    Public,
    /// Any authenticated principal (`/me` answers anonymous callers itself and is `Public`).
    Authenticated,
    /// Authenticated by the cookie session or by a Keycloak or internal JWT bearer. An API token
    /// is refused: a leaked token must not mint further tokens or change the password.
    Interactive,
    /// A repository action; the handler calls [`AppState::authorize`] with the resource.
    Repo(Action),
    /// A project permission; the handler calls [`AppState::authorize_project`].
    Project(ProjectPermission),
    /// An administration area; `nx-admin` passes.
    App(AppArea, Action),
    /// `nx-admin` only.
    Admin,
    /// The module authorizes inside its own engine (the format engine of `/repository` and
    /// `/v2`). The text says where, and is shown in the route catalogue.
    Delegated(&'static str),
}

/// An authenticated caller of any kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatedGrant;

/// An interactive credential: cookie session, internal JWT or Keycloak token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractiveGrant;

/// `nx-admin`, matched by role id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminGrant;

/// A repository action on a named repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoGrant {
    /// Repository name the decision was made for.
    pub repository: String,
    /// The allowed action.
    pub action: Action,
}

/// An administration area action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppGrant {
    /// The area.
    pub area: AppArea,
    /// The action.
    pub action: Action,
}

/// A project permission on one project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectGrant {
    /// The project.
    pub project: ProjectId,
    /// The permission.
    pub permission: ProjectPermission,
}

/// Proof that the caller was asked and allowed. Only the `AppState::authorize*` methods build it.
#[derive(Debug, Clone)]
pub struct Authorized<G> {
    principal: Principal,
    grant: G,
}

impl<G> Authorized<G> {
    /// Private to this module: the one place a proof is made is the `authorize*` family below,
    /// which marks the request as having asked.
    fn new(principal: Principal, grant: G) -> Self {
        note_authorized();
        Self { principal, grant }
    }

    /// The caller the decision was made for.
    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// What was granted.
    #[must_use]
    pub fn grant(&self) -> &G {
        &self.grant
    }
}

fn unauthenticated_or(principal: &Principal, otherwise: ApiError) -> ApiError {
    if principal.is_anonymous() {
        ApiError::unauthenticated()
    } else {
        otherwise
    }
}

impl AppState {
    fn rbac_or_unavailable(&self) -> Result<&Arc<dyn rustryx_core::ports::RbacChecker>, ApiError> {
        self.rbac().ok_or_else(|| {
            ApiError::unavailable("authorization_unavailable")
                .with_source("no authorization checker is wired".into())
        })
    }

    /// Authorizes `action` on `resource` for `principal`, failing closed.
    ///
    /// - no authorization checker wired: 503 (never "allow");
    /// - an anonymous caller while the global anonymous switch is off: 401;
    /// - the checker says deny: 401 for an anonymous caller (so a client asks for
    ///   credentials), 403 for an authenticated one;
    /// - the checker failed: its error, mapped like any other.
    ///
    /// The per-repository anonymous flag and hiding a repository behind a 404 are
    /// the caller's business; this is the deny-by-default floor under them.
    ///
    /// # Errors
    /// An [`ApiError`] for every outcome other than "allowed".
    pub async fn authorize(
        &self,
        principal: &Principal,
        action: Action,
        resource: &ResourceRef,
    ) -> Result<Authorized<RepoGrant>, ApiError> {
        let rbac = self.rbac_or_unavailable()?;
        if principal.is_anonymous() && !self.settings().anonymous_enabled {
            return Err(ApiError::unauthenticated());
        }
        match rbac.check(principal, action, resource).await? {
            Decision::Allow => Ok(Authorized::new(
                principal.clone(),
                RepoGrant {
                    repository: resource.repository.clone(),
                    action,
                },
            )),
            Decision::Deny => Err(unauthenticated_or(principal, ApiError::forbidden())),
        }
    }

    /// Any authenticated principal. Anonymous callers get 401.
    ///
    /// # Errors
    /// 401 for an anonymous caller.
    pub fn authorize_authenticated(
        &self,
        principal: &Principal,
    ) -> Result<Authorized<AuthenticatedGrant>, ApiError> {
        if principal.is_anonymous() {
            return Err(ApiError::unauthenticated());
        }
        Ok(Authorized::new(principal.clone(), AuthenticatedGrant))
    }

    /// An interactive credential: the cookie session or a Keycloak or internal JWT bearer.
    /// Anonymous callers get 401, every other credential (API token, robot or scan token, Basic
    /// password) gets 403.
    ///
    /// # Errors
    /// 401 or 403 as described.
    pub fn authorize_interactive(
        &self,
        principal: &Principal,
    ) -> Result<Authorized<InteractiveGrant>, ApiError> {
        if principal.is_anonymous() {
            return Err(ApiError::unauthenticated());
        }
        // Whitelist, so a credential kind added later is refused until someone decides.
        match principal.auth_method {
            AuthMethod::InternalJwt | AuthMethod::OidcBearer => {
                Ok(Authorized::new(principal.clone(), InteractiveGrant))
            }
            _ => Err(ApiError::forbidden()),
        }
    }

    /// `nx-admin`, decided by `RbacChecker::is_admin` (role id against the store, an
    /// unrestricted token), never by role name.
    ///
    /// # Errors
    /// 503 without a checker, 401 for an anonymous caller, 403 for anyone else.
    pub async fn authorize_admin(
        &self,
        principal: &Principal,
    ) -> Result<Authorized<AdminGrant>, ApiError> {
        let rbac = self.rbac_or_unavailable()?;
        if principal.is_anonymous() {
            return Err(ApiError::unauthenticated());
        }
        if rbac.is_admin(principal).await? {
            Ok(Authorized::new(principal.clone(), AdminGrant))
        } else {
            Err(ApiError::forbidden())
        }
    }

    /// An administration area. `nx-admin` always passes.
    ///
    /// # Errors
    /// 503 without a checker, 401 for an anonymous caller, 403 on a deny.
    pub async fn authorize_app(
        &self,
        principal: &Principal,
        area: AppArea,
        action: Action,
    ) -> Result<Authorized<AppGrant>, ApiError> {
        let rbac = self.rbac_or_unavailable()?;
        if principal.is_anonymous() {
            return Err(ApiError::unauthenticated());
        }
        match rbac.check_app(principal, area, action).await? {
            Decision::Allow => Ok(Authorized::new(
                principal.clone(),
                AppGrant { area, action },
            )),
            Decision::Deny => Err(ApiError::forbidden()),
        }
    }

    /// A project permission.
    ///
    /// # Errors
    /// 503 without a checker, 401 for an anonymous caller, 403 on a deny.
    pub async fn authorize_project(
        &self,
        principal: &Principal,
        project: ProjectId,
        permission: ProjectPermission,
    ) -> Result<Authorized<ProjectGrant>, ApiError> {
        let rbac = self.rbac_or_unavailable()?;
        if principal.is_anonymous() {
            return Err(ApiError::unauthenticated());
        }
        match rbac.check_project(principal, project, permission).await? {
            Decision::Allow => Ok(Authorized::new(
                principal.clone(),
                ProjectGrant {
                    project,
                    permission,
                },
            )),
            Decision::Deny => Err(ApiError::forbidden()),
        }
    }
}

/// The proofs the route layer made, kept in the request extensions for the extractor.
macro_rules! proof_extractor {
    ($grant:ty, $what:literal) => {
        impl<S: Send + Sync> FromRequestParts<S> for Authorized<$grant> {
            type Rejection = ApiError;

            fn from_request_parts(
                parts: &mut Parts,
                _state: &S,
            ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
                std::future::ready(parts.extensions.get::<Self>().cloned().ok_or_else(|| {
                    // The route was not declared with this requirement: a wiring bug, and
                    // failing the request is the only safe answer.
                    ApiError::internal(concat!(
                        "no ",
                        $what,
                        " proof on this request: declare the route with the matching Requires"
                    ))
                }))
            }
        }
    };
}

proof_extractor!(AuthenticatedGrant, "authenticated");
proof_extractor!(InteractiveGrant, "interactive");
proof_extractor!(AdminGrant, "admin");
proof_extractor!(AppGrant, "application area");

#[cfg(test)]
mod tests;
