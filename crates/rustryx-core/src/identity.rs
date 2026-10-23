//! Users, roles, privileges, selectors, tokens and the authenticated principal.

use std::net::IpAddr;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::audit::ActorKind;
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
#[non_exhaustive]
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
    /// An administration area. `Action::Read` is "read", `Write` is "create and update",
    /// `Delete` is "delete". An application grant never applies to content, and a robot or
    /// scan token never holds one.
    Application {
        /// The area.
        area: AppArea,
    },
}

/// An administration area that an application privilege (`app.<area>.read` and `.write`)
/// can name. `nx-admin` passes every area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppArea {
    /// Users.
    Users,
    /// Roles.
    Roles,
    /// Privileges.
    Privileges,
    /// Content selectors.
    Selectors,
    /// Group to role mappings.
    Mappings,
    /// API tokens.
    Tokens,
    /// Blob stores.
    Blobstores,
    /// Tasks and their runs.
    Tasks,
    /// Platform settings.
    Settings,
    /// The audit log.
    Audit,
    /// Scanners.
    Scanners,
    /// Registries.
    Registries,
    /// Replication.
    Replication,
    /// Webhooks.
    Webhooks,
    /// Quotas.
    Quotas,
    /// Garbage collection.
    Gc,
    /// System information and read only mode.
    System,
}

impl AppArea {
    /// Every area, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Users,
        Self::Roles,
        Self::Privileges,
        Self::Selectors,
        Self::Mappings,
        Self::Tokens,
        Self::Blobstores,
        Self::Tasks,
        Self::Settings,
        Self::Audit,
        Self::Scanners,
        Self::Registries,
        Self::Replication,
        Self::Webhooks,
        Self::Quotas,
        Self::Gc,
        Self::System,
    ];

    /// The snake case name used in privilege names (`app.<area>.read`) and in JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Users => "users",
            Self::Roles => "roles",
            Self::Privileges => "privileges",
            Self::Selectors => "selectors",
            Self::Mappings => "mappings",
            Self::Tokens => "tokens",
            Self::Blobstores => "blobstores",
            Self::Tasks => "tasks",
            Self::Settings => "settings",
            Self::Audit => "audit",
            Self::Scanners => "scanners",
            Self::Registries => "registries",
            Self::Replication => "replication",
            Self::Webhooks => "webhooks",
            Self::Quotas => "quotas",
            Self::Gc => "gc",
            Self::System => "system",
        }
    }
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
    #[serde(with = "crate::rfc3339")]
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
    #[serde(default, with = "crate::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// Revocation time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    /// Last successful use.
    #[serde(default, with = "crate::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// How a request was authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
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
    /// A robot token (`robot_tokens`). Rights come from its scopes only.
    RobotToken,
    /// A short lived scan access token handed to an external scanner. Exempt from the pull
    /// gate and from the download counters, bound to one repository.
    ScanToken,
}

/// The authenticated caller of one request.
///
/// The type is `#[non_exhaustive]`: build it with [`Principal::new`] and assign the optional
/// fields, so a field added later is not a breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
    /// Contact address from the identity provider, informational only. Never used to link
    /// accounts or to decide anything.
    pub email: Option<String>,
    /// Human readable name from the identity provider.
    pub display_name: Option<String>,
    /// Directory identity that lets a group refresh skip the user search (the LDAP DN), `None`
    /// otherwise.
    pub external_id: Option<String>,
    /// Expiry of the credential that produced this principal (JWT `exp`, token `expires_at`).
    /// It bounds the lifetime of a cached principal.
    pub expires_at: Option<OffsetDateTime>,
}

impl Principal {
    /// A principal with no id, roles, groups, token data, e-mail, display name, external id or
    /// expiry. `roles` and `external_groups` are filled by the authentication chain.
    #[must_use]
    pub fn new(username: impl Into<String>, source: UserSource, auth_method: AuthMethod) -> Self {
        Self {
            user_id: None,
            username: username.into(),
            source,
            issuer: None,
            subject: None,
            roles: Vec::new(),
            external_groups: Vec::new(),
            groups_confirmed_at: None,
            token_scopes: None,
            token_id: None,
            auth_method,
            email: None,
            display_name: None,
            external_id: None,
            expires_at: None,
        }
    }

    /// The principal of an unauthenticated request. It carries no roles; the
    /// authorizer decides what `nx-anonymous` may do.
    #[must_use]
    pub fn anonymous() -> Self {
        Self::new("anonymous", UserSource::Local, AuthMethod::Anonymous)
    }

    /// True for unauthenticated callers.
    #[must_use]
    pub fn is_anonymous(&self) -> bool {
        self.auth_method == AuthMethod::Anonymous
    }

    /// True when the principal carries a role of that name.
    ///
    /// The match is by NAME and the roles of a principal are references the chain filled in:
    /// the answer is for display and tests, never for an authorization decision. Production
    /// code asks `RbacChecker::is_admin`, which matches the role id against the store. (The
    /// phase 1 `Principal::is_admin` was removed for exactly this reason: a role reference
    /// that only claims the name `nx-admin` passed it.)
    #[must_use]
    pub fn has_role(&self, name: &str) -> bool {
        self.roles.iter().any(|r| r.name == name)
    }

    /// The kind of actor, for events and audit: `Anonymous` for an unauthenticated caller,
    /// `Robot` for a robot or scan token, `Token` for a personal API token, `User` otherwise.
    #[must_use]
    pub fn actor_kind(&self) -> ActorKind {
        match self.auth_method {
            AuthMethod::Anonymous => ActorKind::Anonymous,
            AuthMethod::RobotToken | AuthMethod::ScanToken => ActorKind::Robot,
            AuthMethod::ApiToken => ActorKind::Token,
            AuthMethod::BasicLocal
            | AuthMethod::BasicLdap
            | AuthMethod::InternalJwt
            | AuthMethod::OidcBearer => ActorKind::User,
        }
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
///
/// `#[non_exhaustive]`: build it with [`AuthContext::new`] and the `with_*` methods.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AuthContext {
    /// Peer address after trusted proxy handling, used by the login guard.
    pub client_ip: Option<IpAddr>,
    /// Request id for log correlation.
    pub request_id: Option<String>,
    /// The request deadline. A blocking-work gate that waits for a permit must not wait past
    /// it, so a slow hash cannot outlive the request that asked for it.
    pub deadline: Option<Instant>,
}

impl AuthContext {
    /// A context with no address, request id or deadline.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the peer address.
    #[must_use]
    pub fn with_client_ip(mut self, client_ip: IpAddr) -> Self {
        self.client_ip = Some(client_ip);
        self
    }

    /// Sets the request id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Sets the request deadline.
    #[must_use]
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }
}

/// An authenticator failure together with the method that refused.
///
/// `auth_attempts_total` labels failures with `method`; before this type a failure could only
/// be counted as `unknown`.
#[derive(Debug)]
#[non_exhaustive]
pub struct AuthFailure {
    /// The authentication method that refused, `None` when it cannot be told.
    pub method: Option<AuthMethod>,
    /// What went wrong.
    pub error: crate::error::AuthError,
}

impl AuthFailure {
    /// A failure with the refusing method, when known.
    #[must_use]
    pub fn new(method: Option<AuthMethod>, error: crate::error::AuthError) -> Self {
        Self { method, error }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use super::*;
    use crate::error::AuthError;
    use crate::testing::{assert_wire, at, round_trip_json};

    fn role(name: &str) -> RoleRef {
        RoleRef {
            id: RoleId::generate(),
            name: name.into(),
        }
    }

    #[test]
    fn anonymous_principal_holds_no_role() {
        let p = Principal::anonymous();
        assert!(p.is_anonymous());
        assert!(!p.has_role(ROLE_NX_ADMIN));
        assert_eq!(p.username, "anonymous");
        assert!(p.roles.is_empty() && p.user_id.is_none());
    }

    #[test]
    fn has_role_requires_the_exact_role_name() {
        let mut p = Principal::anonymous();
        p.roles.push(role("nx-admin-ish"));
        assert!(!p.has_role(ROLE_NX_ADMIN));
        p.roles.push(role(ROLE_NX_ADMIN));
        assert!(p.has_role(ROLE_NX_ADMIN));
    }

    #[test]
    fn must_not_decide_administration_by_role_name() {
        // `is_admin` was removed: a name based answer is untrusted, so the only way to ask is
        // `has_role`, whose doc says "display and tests". A principal that merely claims the
        // name still answers true here, which is why `RbacChecker::is_admin` matches ids.
        let mut claimed = Principal::anonymous();
        claimed.roles.push(role(ROLE_NX_ADMIN));
        assert!(claimed.has_role(ROLE_NX_ADMIN));
    }

    #[test]
    fn constructor_leaves_everything_optional_empty() {
        let p = Principal::new("alice", UserSource::Ldap, AuthMethod::BasicLdap);
        assert_eq!(
            (p.username.as_str(), p.source, p.auth_method),
            ("alice", UserSource::Ldap, AuthMethod::BasicLdap)
        );
        assert!(p.user_id.is_none() && p.issuer.is_none() && p.subject.is_none());
        assert!(p.roles.is_empty() && p.external_groups.is_empty());
        assert!(
            p.groups_confirmed_at.is_none() && p.token_scopes.is_none() && p.token_id.is_none()
        );
        assert!(
            p.email.is_none()
                && p.display_name.is_none()
                && p.external_id.is_none()
                && p.expires_at.is_none()
        );
    }

    #[test]
    fn actor_kind_follows_the_authentication_method() {
        use ActorKind::{Anonymous, Robot, Token, User};
        for (method, kind) in [
            (AuthMethod::Anonymous, Anonymous),
            (AuthMethod::BasicLocal, User),
            (AuthMethod::BasicLdap, User),
            (AuthMethod::ApiToken, Token),
            (AuthMethod::InternalJwt, User),
            (AuthMethod::OidcBearer, User),
            (AuthMethod::RobotToken, Robot),
            (AuthMethod::ScanToken, Robot),
        ] {
            let p = Principal::new("x", UserSource::Local, method);
            assert_eq!(p.actor_kind(), kind, "{method:?}");
        }
    }

    #[test]
    fn auth_methods_use_snake_case() {
        assert_wire(&AuthMethod::RobotToken, "\"robot_token\"");
        assert_wire(&AuthMethod::ScanToken, "\"scan_token\"");
        assert_wire(&AuthMethod::OidcBearer, "\"oidc_bearer\"");
        assert_wire(&AuthMethod::BasicLdap, "\"basic_ldap\"");
    }

    #[test]
    fn credentials_debug_hides_password() {
        let c = Credentials::Basic {
            username: "u".into(),
            password: Secret::new("topsecret"),
        };
        assert!(!format!("{c:?}").contains("topsecret"));
        let bearer = Credentials::Bearer(Secret::new("tok-123"));
        assert!(!format!("{bearer:?}").contains("tok-123"));
    }

    #[test]
    fn provider_maps_to_matching_origin() {
        assert_eq!(IdpProvider::Oidc.origin(), RoleOrigin::Oidc);
        assert_eq!(IdpProvider::Ldap.origin(), RoleOrigin::Ldap);
    }

    #[test]
    fn application_privilege_is_a_tagged_grant_with_a_snake_case_area() {
        let grant = PrivilegeGrant::Application {
            area: AppArea::Blobstores,
        };
        assert_wire(&grant, r#"{"type":"application","area":"blobstores"}"#);
        assert_wire(&PrivilegeGrant::Admin, r#"{"type":"admin"}"#);
    }

    /// No wildcard arm: a new area has to be added here and to `ALL`.
    fn area_position(area: AppArea) -> usize {
        match area {
            AppArea::Users => 0,
            AppArea::Roles => 1,
            AppArea::Privileges => 2,
            AppArea::Selectors => 3,
            AppArea::Mappings => 4,
            AppArea::Tokens => 5,
            AppArea::Blobstores => 6,
            AppArea::Tasks => 7,
            AppArea::Settings => 8,
            AppArea::Audit => 9,
            AppArea::Scanners => 10,
            AppArea::Registries => 11,
            AppArea::Replication => 12,
            AppArea::Webhooks => 13,
            AppArea::Quotas => 14,
            AppArea::Gc => 15,
            AppArea::System => 16,
        }
    }

    #[test]
    fn all_lists_every_application_area_exactly_once_in_order() {
        for (index, area) in AppArea::ALL.iter().enumerate() {
            assert_eq!(area_position(*area), index, "{area:?}");
        }
    }

    #[test]
    fn every_application_area_has_a_stable_name() {
        assert_eq!(AppArea::ALL.len(), 17);
        let names: Vec<_> = AppArea::ALL.iter().map(|a| a.as_str()).collect();
        assert_eq!(
            names,
            [
                "users",
                "roles",
                "privileges",
                "selectors",
                "mappings",
                "tokens",
                "blobstores",
                "tasks",
                "settings",
                "audit",
                "scanners",
                "registries",
                "replication",
                "webhooks",
                "quotas",
                "gc",
                "system"
            ]
        );
        for area in AppArea::ALL {
            assert_wire(area, &format!("\"{}\"", area.as_str()));
        }
    }

    #[test]
    fn auth_context_builder_sets_each_field() {
        let deadline = Instant::now() + Duration::from_secs(2);
        let context = AuthContext::new()
            .with_client_ip(IpAddr::V4(Ipv4Addr::LOCALHOST))
            .with_request_id("req-9")
            .with_deadline(deadline);
        assert_eq!(context.client_ip, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert_eq!(context.request_id.as_deref(), Some("req-9"));
        assert_eq!(context.deadline, Some(deadline));
        let empty = AuthContext::new();
        assert!(
            empty.client_ip.is_none() && empty.request_id.is_none() && empty.deadline.is_none()
        );
    }

    #[test]
    fn auth_failure_names_the_method_that_refused() {
        let failure = AuthFailure::new(Some(AuthMethod::ApiToken), AuthError::TokenRevoked);
        assert_eq!(failure.method, Some(AuthMethod::ApiToken));
        assert!(matches!(failure.error, AuthError::TokenRevoked));
        assert!(
            AuthFailure::new(None, AuthError::InvalidCredentials)
                .method
                .is_none()
        );
    }

    #[test]
    fn api_token_serializes_rfc3339_and_reads_a_missing_optional_time() {
        let token = ApiToken {
            id: ApiTokenId::generate(),
            user_id: UserId::generate(),
            name: "ci".into(),
            token_hash: "0".repeat(64),
            display_prefix: "ryx_ab".into(),
            scopes: vec![],
            expires_at: Some(at() + time::Duration::days(30)),
            revoked_at: None,
            last_used_at: None,
            created_at: at(),
        };
        let mut json = round_trip_json(&token);
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["expires_at"], "2026-11-23T10:00:00Z");
        json.as_object_mut().expect("object").remove("revoked_at");
        assert!(serde_json::from_value::<ApiToken>(json).is_ok());
    }

    #[test]
    fn content_selector_serializes_its_update_time() {
        let selector = ContentSelector {
            id: ContentSelectorId::generate(),
            name: "s".into(),
            description: String::new(),
            expression: "format == \"maven2\"".into(),
            updated_at: at(),
        };
        assert_eq!(
            round_trip_json(&selector)["updated_at"],
            "2026-10-24T10:00:00Z"
        );
    }
}
