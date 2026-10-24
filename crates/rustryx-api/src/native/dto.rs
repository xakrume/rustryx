//! Response bodies of the native authentication endpoints, named after the schemas of
//! `docs/openapi/rustryx-api-v1.yaml`.
//!
//! Rules for every type in this file, each pinned by a test:
//!
//! - no `Secret` (the core type serializes its real value) and no `token_hash`: a response
//!   carries `SecretStatus` for a write only secret, or nothing; the test
//!   `must_not_embed_the_secret_type_in_a_response_dto` reads this very file;
//! - timestamps are RFC 3339 through `rustryx_core::rfc3339`;
//! - a nullable field is always present, with `null` for "none" (api-conventions section 2).

use rustryx_core::identity::{Action, AuthMethod, RoleOrigin, UserSource};
use rustryx_core::ids::{RoleId, UserId};
use rustryx_core::model::RepoFormat;
use serde::Serialize;
use time::OffsetDateTime;

/// `UserRef`: a small reference to a user.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct UserRef {
    pub id: UserId,
    pub username: String,
    pub display_name: Option<String>,
    pub source: UserSource,
}

/// `Session`: the state of the cookie session. Always answered with 200.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionView {
    pub authenticated: bool,
    pub user: Option<UserRef>,
    pub auth_method: Option<AuthMethod>,
    pub csrf_token: Option<String>,
    #[serde(with = "rustryx_core::rfc3339::option")]
    pub issued_at: Option<OffsetDateTime>,
    #[serde(with = "rustryx_core::rfc3339::option")]
    pub idle_expires_at: Option<OffsetDateTime>,
    #[serde(with = "rustryx_core::rfc3339::option")]
    pub absolute_expires_at: Option<OffsetDateTime>,
    pub idle_timeout_secs: u64,
    pub absolute_timeout_secs: u64,
}

/// `CsrfToken`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CsrfTokenView {
    pub csrf_token: String,
}

/// `LogoutResult`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LogoutResult {
    pub redirect_url: Option<String>,
}

/// One flag of the sign-in options.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Enabled {
    pub enabled: bool,
}

/// The SSO entry of the sign-in options.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct OidcProvider {
    pub enabled: bool,
    pub available: bool,
    pub display_name: Option<String>,
    pub login_path: Option<String>,
}

/// `LoginProviders`: what the login page offers. Reveals nothing beyond what the page shows.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LoginProviders {
    pub local: Enabled,
    pub ldap: Enabled,
    pub oidc: OidcProvider,
}

/// `RoleRef`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RoleRefView {
    pub id: RoleId,
    pub name: String,
}

/// `EffectiveRole`: a role the caller holds, with where it comes from.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct EffectiveRole {
    pub role: RoleRefView,
    pub origin: RoleOrigin,
    pub groups: Vec<String>,
}

/// The `user` object of `MeView`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MeUser {
    pub id: UserId,
    pub username: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub source: UserSource,
    pub issuer: Option<String>,
    pub subject: Option<String>,
    pub enabled: bool,
    #[serde(with = "rustryx_core::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The `token` object of `MeView`: set when the request used an API token. The token itself
/// is never here, and neither is its hash.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MeToken {
    pub id: rustryx_core::ids::ApiTokenId,
    pub name: String,
    pub scopes: Vec<String>,
}

/// `RepositoryGrantSummary`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepositoryGrantSummary {
    pub privilege: String,
    pub repository: String,
    pub format: Option<RepoFormat>,
    pub actions: Vec<Action>,
    pub selector: bool,
}

/// The `can` object of `PermissionSummary`.
// Five flags because the wire shape of `PermissionSummary.can` is fixed by the OpenAPI schema.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Can {
    pub create_repository: bool,
    pub upload: bool,
    pub create_personal_token: bool,
    pub view_audit: bool,
    pub administer: bool,
}

/// `PermissionSummary`: for the UI to show or hide entries. Hidden is a convenience, not
/// security: the server decides every request on its own.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PermissionSummary {
    pub is_admin: bool,
    pub admin_areas: Vec<&'static str>,
    pub repository_grants: Vec<RepositoryGrantSummary>,
    pub can: Can,
}

/// `TokenPolicy`.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct TokenPolicyView {
    pub default_ttl_days: u32,
    pub max_ttl_days: u32,
    pub require_expiry: bool,
}

/// The `system` object of `MeView`.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct MeSystem {
    pub read_only: bool,
    pub anonymous_enabled: bool,
    pub base_url_configured: bool,
}

/// `MeView`: the calling principal.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MeView {
    pub authenticated: bool,
    pub user: Option<MeUser>,
    pub auth_method: AuthMethod,
    pub roles: Vec<EffectiveRole>,
    pub groups: Vec<String>,
    #[serde(with = "rustryx_core::rfc3339::option")]
    pub groups_confirmed_at: Option<OffsetDateTime>,
    pub token: Option<MeToken>,
    pub permissions: PermissionSummary,
    pub token_policy: TokenPolicyView,
    pub system: MeSystem,
}

#[cfg(test)]
mod tests;
