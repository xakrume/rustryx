//! Group handling shared by both identity providers: the group to role mapping
//! ([`RoleMapper`]) and just-in-time account provisioning ([`JitProvisioner`]).
//!
//! The providers themselves (Keycloak claims, LDAP search) only report group ids
//! through `GroupResolver`; everything that turns ids into rights lives here, so
//! the exact-match and "only `admin_groups` makes an administrator" rules have a
//! single implementation.

#![allow(clippy::missing_errors_doc)] // errors are `AuthError` variants documented on the types

mod jit;
mod mapper;

pub use self::jit::{DEFAULT_PERSIST_INTERVAL, JitConfig, JitMode, JitProvisioner};
pub use self::mapper::{
    DEFAULT_SNAPSHOT_TTL, ExternalRoles, MAX_EXTERNAL_GROUPS, MappingOutcome, ProviderPolicy,
    RoleMapper, RoleMapperConfig,
};
