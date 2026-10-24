//! HTTP layer of Rustryx: router, middleware stack, error envelope, probes.
//!
//! This crate owns everything that is common to every endpoint and nothing that is
//! specific to one. Features plug in through [`RouteModule`] (see `router.rs`),
//! shared dependencies enter through [`AppState`], and every failure leaves as an
//! [`ApiError`] envelope with a stable public code.
//!
//! What it guarantees to the routes mounted in it:
//!
//! - a request id on every response, in the logs and in error bodies;
//! - an authenticated (or anonymous) [`Principal`](rustryx_core::identity::Principal)
//!   in the request extensions, or a 401/503 before the handler runs;
//! - limits (body size, idle time, response time, concurrency, request rate per
//!   client) with 413, 408, 504, 503 and 429 plus `Retry-After` as the answers;
//! - security headers, panic recovery (the node survives a panicking handler) and
//!   metrics labelled by route template.
//!
//! - deny by default as structure: a route is declared with a [`Requires`], an `Authorized`
//!   proof can only come from `AppState::authorize*`, and a handler that answers without having
//!   asked is overruled (see `authz.rs`);
//! - the cookie session of the web UI with CSRF and `Origin` checks, and the native
//!   authentication endpoints under `/api/v1/auth` and `/api/v1/me` ([`NativeAuthModule`]).
//!
//! What it does not do: authorization decisions (the handlers call `AppState::authorize*`), the
//! other native API resources (separate modules mounted through [`RouteModule`], see
//! `docs/server.md` for the template) and any compat or format handler.

// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// `CatchPanicLayer` can only turn a panic into a 500 when panics unwind. Under
// `panic = "abort"` one bad request would kill the whole node, so such a build is
// refused outright rather than discovered in production.
#[cfg(panic = "abort")]
compile_error!("rustryx-api requires panic = \"unwind\": panic recovery depends on it");

mod auth;
mod authz;
mod client_ip;
mod error;
mod extract;
mod health;
mod https;
mod layers;
mod native;
mod ports;
mod rate_limit;
mod readiness;
mod request_id;
mod router;
mod routes;
mod session;
mod state;

#[cfg(test)]
mod session_support;
#[cfg(test)]
pub(crate) use test_support::resource as resource_for_tests;
#[cfg(test)]
mod test_support;

pub use auth::{
    AuthenticatedPrincipal, RequestClientIp, RequestPrincipal, credentials_from_headers,
};
pub use authz::{
    AdminGrant, AppGrant, AuthenticatedGrant, Authorized, InteractiveGrant, ProjectGrant,
    RepoGrant, Requires,
};
pub use client_ip::{ClientIp, resolve_client_ip};
pub use error::{ApiError, Challenge, DEFAULT_RETRY_AFTER};
pub use extract::{ApiJson, ApiPath, ApiQuery};
pub use https::OriginPolicy;
pub use layers::record_repo;
pub use native::{AuthApiDeps, AuthApiSettings, NativeAuthModule, TokenPolicy};
pub use ports::{
    GroupSnapshot, IssuedSession, OidcCallback, OidcCompletion, OidcLogin, OidcLoginPort,
    OidcStart, SessionIssuer, SessionRequest, SsoRefreshRequest, SsoRefreshed, SsoSessionRefresher,
};
pub use rate_limit::{ClientKey, RateLimiter};
pub use readiness::{
    Advisory, NotWiredProbe, Outcome, ProbeReport, ReadinessCache, ReadinessProbe, SchemaStatus,
};
pub use request_id::{REQUEST_ID_HEADER, RequestId, current_request_id};
pub use router::{RouteModule, RouterBuilder, build_router};
pub use routes::{RouteCatalog, RouteDecl, RouteSet};
pub use session::{
    CSRF_COOKIE, CSRF_HEADER, CsrfKey, CsrfKeyError, DEFAULT_REVOKE_GRACE, MAX_REVOKE_GRACE,
    SESSION_COOKIE, SSO_COOKIE, SessionManager, SessionSettings,
};
pub use state::{
    ApiSettings, AppState, ChallengeSelector, Lifecycle, MetricsAccess, RateLimitSettings,
    RegistryBearerTokens,
};
