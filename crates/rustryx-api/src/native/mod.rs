//! The native authentication endpoints under `/api/v1`: sign-in options, password login, the
//! OIDC redirect flow, logout, the session, the CSRF token and `/me`.
//!
//! Everything else of the native API (repositories, users, roles, tokens, ...) is built by
//! separate modules against `docs/openapi/rustryx-api-v1.yaml`. This module is also their
//! worked example: it shows how a module builds its state from `RouteSet::app_state`, declares
//! each route with its [`Requires`](crate::Requires), takes the proof of an up-front
//! requirement as an extractor, and keeps its response types in one file that a test reads
//! (`dto.rs`). `docs/server.md` has the short version.
//!
//! | Route | Requires |
//! |---|---|
//! | `GET /api/v1/auth/providers` | `Public` |
//! | `POST /api/v1/auth/login` | `Public` (the origin check is its CSRF defence) |
//! | `GET /api/v1/auth/oidc/login`, `GET /api/v1/auth/oidc/callback` | `Public` |
//! | `POST /api/v1/auth/logout` | `Public` (without a session it only clears cookies) |
//! | `GET /api/v1/auth/session` | `Public` (an anonymous caller gets `authenticated: false`) |
//! | `POST /api/v1/auth/session/refresh` | `Interactive` |
//! | `GET /api/v1/auth/csrf` | `Interactive` |
//! | `GET /api/v1/me` | `Public` (an anonymous caller gets what `nx-anonymous` may see) |

mod audit;
mod dto;
mod login;
mod me;
mod oidc;
mod requests;
mod session_endpoints;

use std::sync::Arc;

use axum::routing::{get, post};
use rustryx_core::ports::{AuditSink, Clock, RoleRepo, TokenRepo, UserRepo};

use crate::authz::Requires;
use crate::ports::OidcLoginPort;
use crate::router::RouteModule;
use crate::routes::RouteSet;
use crate::state::AppState;

/// Lifetime rules for personal tokens, as `/me` reports them (the rules themselves are enforced
/// by `rustryx-auth`'s token creation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenPolicy {
    /// Lifetime given when none is asked for.
    pub default_ttl_days: u32,
    /// The longest lifetime.
    pub max_ttl_days: u32,
    /// Whether a token must expire.
    pub require_expiry: bool,
}

impl Default for TokenPolicy {
    fn default() -> Self {
        Self {
            default_ttl_days: 90,
            max_ttl_days: 365,
            require_expiry: true,
        }
    }
}

/// Settings of the authentication endpoints.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AuthApiSettings {
    /// LDAP login is on (the username and password form serves it).
    pub ldap_enabled: bool,
    /// Label of the SSO button.
    pub oidc_display_name: Option<String>,
    /// `server.base_url` is set.
    pub base_url_configured: bool,
    /// Token lifetime rules shown by `/me`.
    pub token_policy: TokenPolicy,
}

/// What the authentication endpoints are built from.
#[non_exhaustive]
pub struct AuthApiDeps {
    /// Accounts.
    pub users: Arc<dyn UserRepo>,
    /// Roles, for the repository grants `/me` lists.
    pub roles: Arc<dyn RoleRepo>,
    /// API tokens, for the name of the token a request used.
    pub tokens: Arc<dyn TokenRepo>,
    /// The audit trail of sign-ins and sign-outs.
    pub audit: Arc<dyn AuditSink>,
    /// Time source of audit events.
    pub clock: Arc<dyn Clock>,
    /// The SSO login, `None` when OIDC is off.
    pub oidc: Option<Arc<dyn OidcLoginPort>>,
    /// Settings.
    pub settings: AuthApiSettings,
}

impl AuthApiDeps {
    /// Dependencies with default settings and no SSO.
    #[must_use]
    pub fn new(
        users: Arc<dyn UserRepo>,
        roles: Arc<dyn RoleRepo>,
        tokens: Arc<dyn TokenRepo>,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            users,
            roles,
            tokens,
            audit,
            clock,
            oidc: None,
            settings: AuthApiSettings::default(),
        }
    }
}

/// The state of the handlers: the shared state plus this module's own ports.
#[derive(Clone)]
pub(crate) struct AuthApi {
    pub(crate) app: AppState,
    pub(crate) users: Arc<dyn UserRepo>,
    pub(crate) roles: Arc<dyn RoleRepo>,
    pub(crate) tokens: Arc<dyn TokenRepo>,
    pub(crate) audit: Arc<dyn AuditSink>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) oidc: Option<Arc<dyn OidcLoginPort>>,
    pub(crate) settings: Arc<AuthApiSettings>,
}

/// The module that mounts the endpoints.
pub struct NativeAuthModule {
    deps: AuthApiDeps,
}

impl NativeAuthModule {
    /// A module over these dependencies.
    #[must_use]
    pub fn new(deps: AuthApiDeps) -> Self {
        Self { deps }
    }
}

impl RouteModule for NativeAuthModule {
    fn name(&self) -> &'static str {
        "native-auth"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let api = AuthApi {
            app: routes.app_state().clone(),
            users: self.deps.users.clone(),
            roles: self.deps.roles.clone(),
            tokens: self.deps.tokens.clone(),
            audit: self.deps.audit.clone(),
            clock: self.deps.clock.clone(),
            oidc: self.deps.oidc.clone(),
            settings: Arc::new(self.deps.settings.clone()),
        };
        routes
            .route(
                "/api/v1/auth/providers",
                Requires::Public,
                get(login::providers).with_state(api.clone()),
            )
            .route(
                "/api/v1/auth/login",
                Requires::Public,
                post(login::login).with_state(api.clone()),
            )
            .route(
                "/api/v1/auth/oidc/login",
                Requires::Public,
                get(oidc::start).with_state(api.clone()),
            )
            .route(
                "/api/v1/auth/oidc/callback",
                Requires::Public,
                get(oidc::callback).with_state(api.clone()),
            )
            .route(
                "/api/v1/auth/logout",
                Requires::Public,
                post(login::logout).with_state(api.clone()),
            )
            .route(
                "/api/v1/auth/session",
                Requires::Public,
                get(session_endpoints::session).with_state(api.clone()),
            )
            .route(
                "/api/v1/auth/session/refresh",
                Requires::Interactive,
                post(session_endpoints::refresh).with_state(api.clone()),
            )
            .route(
                "/api/v1/auth/csrf",
                Requires::Interactive,
                get(session_endpoints::csrf).with_state(api.clone()),
            )
            .route("/api/v1/me", Requires::Public, get(me::me).with_state(api))
    }
}

#[cfg(test)]
mod tests;
