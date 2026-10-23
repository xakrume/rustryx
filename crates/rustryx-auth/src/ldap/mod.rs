//! LDAP: `LDAPS` or `StartTLS` only, bind authentication, group search.
//!
//! Entry points for the composition root:
//! - [`LdapConfig`] is the serde shape of `auth.ldap`; `validate()` it at startup;
//! - [`LdapService::new`] builds the shared state (TLS trust, pool, group cache);
//! - [`LdapAuthenticator`] implements `Authenticator` for Basic credentials;
//! - [`LdapGroupResolver`] implements `GroupResolver` and returns DNs unmodified.
//!
//! The rules live above the [`LdapTransport`] seam and are tested with a fake that
//! never opens a socket; `net.rs` is the thin `ldap3` implementation. See
//! `docs/ldap.md` for the configuration reference and the decisions behind it.

// Every fallible function here returns the error type named in its signature
// (`LdapConfigError`, `AuthError`, `TransportError`), and the type documents its
// variants; a per-function `# Errors` section would repeat that without adding facts.
#![allow(clippy::missing_errors_doc)]

mod authenticator;
mod breaker;
mod cache;
pub mod config;
pub mod dn;
mod error;
pub mod filter;
mod groups;
mod input;
mod net;
mod pool;
mod resolver;
mod service;
mod stats;
mod tls;
pub mod transport;
mod user;

#[cfg(test)]
mod auth_tests;
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod group_tests;
#[cfg(test)]
mod hint_tests;
#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod log_tests;
#[cfg(test)]
mod pool_tests;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod testing_server;
#[cfg(test)]
mod testing_tls;
#[cfg(test)]
mod wire_tests;

pub use authenticator::LdapAuthenticator;
pub use config::{LdapConfig, LdapConfigError, LdapGroupStrategy};
pub use dn::escape_dn_value;
pub use filter::{FilterTemplateError, escape_filter_value};
pub use resolver::{GroupFreshness, LdapGroupResolver, ResolvedGroups};
pub use service::{LdapInitError, LdapService};
pub use stats::{LdapStats, LdapStatsSnapshot};
pub use tls::TlsSetupError;
pub use transport::{
    LdapConnection, LdapEntry, LdapTransport, SearchRequest, SearchResponse, SearchScope,
    TransportError,
};
