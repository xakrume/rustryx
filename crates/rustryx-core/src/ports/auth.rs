//! Authentication, group resolution and authorization ports.
//!
//! `rustryx-auth` answers "who is this" and "which external groups does that
//! identity have". `rustryx-rbac` answers "may this principal do that". The two
//! never mix: authenticators make no authorization decisions.

use async_trait::async_trait;

use crate::error::AuthError;
use crate::identity::{Action, AuthContext, Credentials, IdpProvider, Principal};
use crate::model::RepoFormat;

/// Verifies credentials and produces a principal.
#[async_trait]
pub trait Authenticator: Send + Sync {
    /// Cancel safe.
    ///
    /// Returns `Ok(None)` when the credentials are not applicable to this
    /// authenticator (for example an OIDC authenticator given a Basic header),
    /// so the chain can try the next one. Returns `Err` when they are applicable
    /// and wrong, expired, locked out or when a required backend failed.
    /// Roles are not resolved here: the returned principal has `external_groups`
    /// filled where the credential carries them, and `roles` is filled later.
    async fn authenticate(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError>;
}

/// External groups of an authenticated identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalGroups {
    /// Provider that produced the list.
    pub provider: IdpProvider,
    /// Group ids: full Keycloak group paths or LDAP DNs. Case sensitive, unmodified.
    pub groups: Vec<String>,
}

/// One interface over the Keycloak `groups` claim and LDAP group search.
/// The mapping group -> role is shared and lives in `rustryx-auth`
/// (`RoleMapper`, backed by `idp_group_mappings`).
#[async_trait]
pub trait GroupResolver: Send + Sync {
    /// Cancel safe.
    ///
    /// Returns the groups for the principal's current request. A failed or timed
    /// out lookup is an `Err(AuthError::GroupLookupFailed)`, never an empty list:
    /// callers fail closed. A missing or empty OIDC `groups` claim is a valid
    /// empty result and removes roles of origin `oidc` for this request.
    async fn external_groups(&self, principal: &Principal) -> Result<ExternalGroups, AuthError>;
}

/// What is being accessed, as seen by authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRef {
    /// Repository name.
    pub repository: String,
    /// Repository format.
    pub format: RepoFormat,
    /// Normalized path produced by the format's `classify`.
    pub path: String,
    /// Component coordinate when the format can derive one before reading the body.
    pub coordinate: Option<Coordinate>,
}

/// Format neutral component coordinate exposed to content selectors as `coordinate.*`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coordinate {
    /// Namespace (groupId, scope).
    pub group: Option<String>,
    /// Package name.
    pub name: Option<String>,
    /// Version.
    pub version: Option<String>,
}

/// Result of an authorization check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Allowed.
    Allow,
    /// Not allowed (also when the information needed to decide is missing).
    Deny,
}

/// Authorization: effective privileges and content selectors.
#[async_trait]
pub trait RbacChecker: Send + Sync {
    /// Cancel safe.
    ///
    /// Allows when at least one privilege of the principal's roles (nested roles
    /// expanded) grants `action` on the resource and, for selector privileges,
    /// the selector matches. There are no deny rules. Anonymous principals are
    /// checked as `nx-anonymous`; anonymous write is always denied.
    async fn check(
        &self,
        principal: &Principal,
        action: Action,
        resource: &ResourceRef,
    ) -> Result<Decision, AuthError>;

    /// Cancel safe. True when the principal may browse at least something in
    /// the repository. Used to hide repositories (404 instead of 403).
    async fn can_see_repository(
        &self,
        principal: &Principal,
        repository: &str,
        format: RepoFormat,
    ) -> Result<bool, AuthError>;
}
