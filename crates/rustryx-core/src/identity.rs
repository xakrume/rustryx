//! Users, roles, privileges, selectors, tokens and the authenticated principal.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{ApiTokenId, ContentSelectorId, IdpGroupMappingId, PrivilegeId, RoleId, UserId};
use crate::model::RepoFormat;
use crate::secret::Secret;

/// Name of the only administrative role. Assigned only by exact match of a full
/// group path (OIDC) or group DN (LDAP) against the configured admin groups.
pub const ROLE_NX_ADMIN: &str = "nx-admin";
/// Built-in role of unauthenticated callers. Grants no write access by itself.
pub const ROLE_NX_ANONYMOUS: &str = "nx-anonymous";
/// Built-in role for developers: read and write on repositories.
pub const ROLE_NX_DEVELOPER: &str = "nx-developer";
/// Prefix of API tokens. The stored value is the SHA-256 of the whole token.
pub const API_TOKEN_PREFIX: &str = "ryx_";

/// Where an account comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserSource {
    /// Local account with a password hash.
    Local,
    /// LDAP or Active Directory.
    Ldap,
    /// OIDC provider (Keycloak). Keyed by `(issuer, subject)`.
    Oidc,
}

/// A user account.
#[derive(Debug, Clone)]
pub struct User {
    /// Identifier.
    pub id: UserId,
    /// Login name. Unique across all sources; a clash between sources is a conflict.
    pub username: String,
    /// Account source.
    pub source: UserSource,
    /// OIDC issuer, only for [`UserSource::Oidc`].
    pub issuer: Option<String>,
    /// OIDC subject, only for [`UserSource::Oidc`]. Never joined with the issuer.
    pub subject: Option<String>,
    /// Contact address, informational only. Never used to link accounts.
    pub email: Option<String>,
    /// Human readable name.
    pub display_name: Option<String>,
    /// Disabled accounts cannot authenticate.
    pub enabled: bool,
    /// PHC string (argon2id) or an imported bcrypt hash. Only for local accounts.
    pub password_hash: Option<Secret>,
    /// Internal JWTs issued before this instant are rejected.
    pub tokens_valid_after: Option<OffsetDateTime>,
    /// Creation time.
    pub created_at: OffsetDateTime,
}

/// Input for creating an account.
#[derive(Debug, Clone)]
pub struct NewUser {
    /// Login name.
    pub username: String,
    /// Account source.
    pub source: UserSource,
    /// OIDC issuer.
    pub issuer: Option<String>,
    /// OIDC subject.
    pub subject: Option<String>,
    /// Contact address.
    pub email: Option<String>,
    /// Human readable name.
    pub display_name: Option<String>,
    /// Password hash for local accounts.
    pub password_hash: Option<Secret>,
}

/// Who put a role on a user. Synchronisation from an identity provider replaces
/// only rows of its own origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleOrigin {
    /// Assigned by an administrator.
    Manual,
    /// Derived from Keycloak groups.
    Oidc,
    /// Derived from LDAP groups.
    Ldap,
}

/// Lightweight role reference carried in a [`Principal`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoleRef {
    /// Role id.
    pub id: RoleId,
    /// Role name.
    pub name: String,
}

/// A role: a set of privileges and nested roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    /// Identifier.
    pub id: RoleId,
    /// Unique name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Built-in roles cannot be edited or deleted.
    pub builtin: bool,
    /// Directly attached privileges.
    pub privilege_ids: Vec<PrivilegeId>,
    /// Nested roles (cycles are rejected on write).
    pub nested_role_ids: Vec<RoleId>,
}

/// An action on content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// See that the repository or path exists (listings).
    Browse,
    /// Download.
    Read,
    /// Upload or overwrite.
    Write,
    /// Remove.
    Delete,
}

/// What a privilege applies to. Privileges only allow, there is no deny.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PrivilegeGrant {
    /// Every action on everything, including administration.
    Admin,
    /// Actions on a repository (or `*`) restricted to one format (or any).
    RepositoryView {
        /// Format restriction, `None` means any.
        format: Option<RepoFormat>,
        /// Repository name or `*`.
        repository: String,
    },
    /// Actions on the part of a repository matched by a content selector.
    RepositoryContentSelector {
        /// Repository name or `*`.
        repository: String,
        /// Selector deciding which paths and coordinates match.
        selector_id: ContentSelectorId,
    },
}

/// A privilege.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Privilege {
    /// Identifier.
    pub id: PrivilegeId,
    /// Unique name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Built-in privileges cannot be edited or deleted.
    pub builtin: bool,
    /// Target of the privilege.
    pub grant: PrivilegeGrant,
    /// Allowed actions.
    pub actions: Vec<Action>,
}

/// A content selector: a boolean expression over `format`, `path` and
/// `coordinate.*` (CEL subset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSelector {
    /// Identifier.
    pub id: ContentSelectorId,
    /// Unique name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Expression source, compiled by the `SelectorEngine` in `rustryx-rbac`.
    pub expression: String,
    /// Last change time, part of the compiled-program cache key.
    pub updated_at: OffsetDateTime,
}

/// Identity provider kind for group mappings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdpProvider {
    /// Keycloak group claim (full group path).
    Oidc,
    /// LDAP group (distinguished name).
    Ldap,
}

impl IdpProvider {
    /// The role origin written for roles derived from this provider.
    #[must_use]
    pub const fn origin(self) -> RoleOrigin {
        match self {
            Self::Oidc => RoleOrigin::Oidc,
            Self::Ldap => RoleOrigin::Ldap,
        }
    }
}

/// Mapping of an external group to a role. Stored in the database, the YAML
/// configuration only seeds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdpGroupMapping {
    /// Identifier.
    pub id: IdpGroupMappingId,
    /// Provider the group belongs to.
    pub provider: IdpProvider,
    /// Exact, case sensitive group id (Keycloak path with leading slash, or LDAP DN).
    pub external_group: String,
    /// Role granted by membership.
    pub role_id: RoleId,
    /// Evaluation order, lower first.
    pub priority: i32,
}

/// An API token record. The secret itself is shown once and never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiToken {
    /// Identifier.
    pub id: ApiTokenId,
    /// Owner.
    pub user_id: UserId,
    /// Human readable label.
    pub name: String,
    /// Lowercase hex SHA-256 of the full token string.
    pub token_hash: String,
    /// First characters of the token, safe to display.
    pub display_prefix: String,
    /// Optional restriction of actions or repositories (empty means owner's rights).
    pub scopes: Vec<String>,
    /// Expiry, `None` means no expiry.
    pub expires_at: Option<OffsetDateTime>,
    /// Revocation time.
    pub revoked_at: Option<OffsetDateTime>,
    /// Last successful use.
    pub last_used_at: Option<OffsetDateTime>,
    /// Creation time.
    pub created_at: OffsetDateTime,
}

/// How a request was authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// No credentials.
    Anonymous,
    /// Basic with a local password.
    BasicLocal,
    /// Basic with an LDAP bind.
    BasicLdap,
    /// API token `ryx_*`.
    ApiToken,
    /// Internal JWT issued after the UI login.
    InternalJwt,
    /// Keycloak access token validated through JWKS.
    OidcBearer,
}

/// The authenticated caller of one request.
#[derive(Debug, Clone)]
pub struct Principal {
    /// Account id. `None` for anonymous callers and before JIT provisioning.
    pub user_id: Option<UserId>,
    /// Login name (`anonymous` for anonymous callers).
    pub username: String,
    /// Account source.
    pub source: UserSource,
    /// OIDC issuer (OIDC callers only).
    pub issuer: Option<String>,
    /// OIDC subject (OIDC callers only).
    pub subject: Option<String>,
    /// Roles effective for this request, after group resolution.
    pub roles: Vec<RoleRef>,
    /// External group ids that produced `roles` (Keycloak paths or LDAP DNs).
    pub external_groups: Vec<String>,
    /// When external membership was last confirmed by the provider (LDAP: at most 5 minutes old).
    pub groups_confirmed_at: Option<OffsetDateTime>,
    /// Scopes of the API token used, if any.
    pub token_scopes: Option<Vec<String>>,
    /// API token record used, if any.
    pub token_id: Option<ApiTokenId>,
    /// How the request was authenticated.
    pub auth_method: AuthMethod,
}

impl Principal {
    /// The principal of an unauthenticated request. It carries no roles; the
    /// authorizer decides what `nx-anonymous` may do.
    #[must_use]
    pub fn anonymous() -> Self {
        Self {
            user_id: None,
            username: "anonymous".to_owned(),
            source: UserSource::Local,
            issuer: None,
            subject: None,
            roles: Vec::new(),
            external_groups: Vec::new(),
            groups_confirmed_at: None,
            token_scopes: None,
            token_id: None,
            auth_method: AuthMethod::Anonymous,
        }
    }

    /// True for unauthenticated callers.
    #[must_use]
    pub fn is_anonymous(&self) -> bool {
        self.auth_method == AuthMethod::Anonymous
    }

    /// True when the principal holds a role of that name.
    #[must_use]
    pub fn has_role(&self, name: &str) -> bool {
        self.roles.iter().any(|r| r.name == name)
    }

    /// True when the principal holds `nx-admin`.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.has_role(ROLE_NX_ADMIN)
    }
}

/// Credentials extracted from a request, before verification.
#[derive(Debug, Clone)]
pub enum Credentials {
    /// Nothing was presented.
    Anonymous,
    /// `Authorization: Basic`. The password may be an API token (`ryx_*`).
    Basic {
        /// User name.
        username: String,
        /// Password or API token.
        password: Secret,
    },
    /// `Authorization: Bearer`: API token, internal JWT or Keycloak access token.
    Bearer(Secret),
}

/// Request facts authenticators need besides the credentials.
#[derive(Debug, Clone, Default)]
pub struct AuthContext {
    /// Peer address after trusted proxy handling, used by the login guard.
    pub client_ip: Option<IpAddr>,
    /// Request id for log correlation.
    pub request_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_principal_is_not_admin() {
        let p = Principal::anonymous();
        assert!(p.is_anonymous());
        assert!(!p.is_admin());
    }

    #[test]
    fn admin_requires_exact_role_name() {
        let mut p = Principal::anonymous();
        p.roles.push(RoleRef {
            id: RoleId::generate(),
            name: "nx-admin-ish".into(),
        });
        assert!(!p.is_admin());
        p.roles.push(RoleRef {
            id: RoleId::generate(),
            name: ROLE_NX_ADMIN.into(),
        });
        assert!(p.is_admin());
    }

    #[test]
    fn credentials_debug_hides_password() {
        let c = Credentials::Basic {
            username: "u".into(),
            password: Secret::new("topsecret"),
        };
        assert!(!format!("{c:?}").contains("topsecret"));
    }

    #[test]
    fn provider_maps_to_matching_origin() {
        assert_eq!(IdpProvider::Oidc.origin(), RoleOrigin::Oidc);
        assert_eq!(IdpProvider::Ldap.origin(), RoleOrigin::Ldap);
    }
}
