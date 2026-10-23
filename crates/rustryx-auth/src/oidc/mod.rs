//! OIDC (Keycloak): discovery, JWKS cache, bearer validation, authorization code + PKCE.
//!
//! Wiring for the composition root:
//!
//! ```ignore
//! let provider = OidcProvider::new(config)?;          // never contacts Keycloak
//! let background = provider.spawn_background(shutdown.clone());
//! let authenticator = provider.bearer_authenticator(); // impl Authenticator
//! let resolver = provider.group_resolver();            // impl GroupResolver
//! let flow = provider.login_flow();                    // Option<OidcLoginFlow>
//! let status = provider.status();                      // metrics and flags
//! ```
//!
//! The normative behaviour is README sections 5, 8.3 and 8.5; `docs/oidc.md` lists how each
//! rule is enforced and tested.

// Every `Result` here is one of the typed errors of this module or `AuthError`; repeating
// "# Errors" on each method adds noise without information.
#![allow(clippy::missing_errors_doc)]
// Names such as `OidcConfig` inside `oidc` are what callers import; stripping the prefix
// would make `config::Config` ambiguous at the use site.
#![allow(clippy::module_name_repetitions)]

mod bearer;
mod claims;
mod config;
mod cookie;
mod csrf;
mod discovery;
mod http;
mod jwks;
mod login_flow;
mod provider;
mod validator;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use bearer::{OidcBearerAuthenticator, OidcGroupResolver, VerifiedBearer};
pub use claims::OidcIdentity;
pub use config::{
    DEFAULT_FLOW_COOKIE_NAME, DEFAULT_GROUPS_CLAIM, DEFAULT_MAX_GROUP_BYTES, DEFAULT_MAX_GROUPS,
    DEFAULT_MAX_TOKEN_BYTES, MAX_CLOCK_SKEW, MAX_FLOW_TTL, MAX_JWKS_TTL, MIN_JWKS_REFRESH_INTERVAL,
    OidcConfig, OidcConfigError, UiClientConfig,
};
pub use cookie::{OpenError, SealError, SealedCookie};
pub use csrf::{generate_csrf_token, verify_csrf_token};
pub use login_flow::{LoginStart, OidcLoginFlow};
pub use provider::{OidcInitError, OidcProvider, OidcStatus};
