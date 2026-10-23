//! Metadata store ports (implemented by `rustryx-db` on PostgreSQL).
//!
//! All methods are cancel safe: each is one statement or one transaction, so a
//! cancelled future leaves either the old or the new state, never a mix.
//! Errors are [`StoreError`]: `NotFound`, `Conflict` (unique violation),
//! `Unavailable`, `Backend`.

use std::net::IpAddr;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::StoreError;
use crate::identity::{
    ApiToken, ContentSelector, IdpGroupMapping, IdpProvider, NewUser, Privilege, Role, RoleOrigin,
    RoleRef, User, UserSource,
};
use crate::ids::{
    ApiTokenId, AssetId, BlobId, BlobStoreId, ComponentId, ContentSelectorId, IdpGroupMappingId,
    PrivilegeId, RepositoryId, RoleId, UserId,
};
use crate::model::{
    Asset, BlobRecord, BlobStoreDef, Component, NewAsset, NewComponent, PublishOutcome, Repository,
};
use crate::names::RepoName;

/// Keyset pagination request. `after` is the opaque `next` of the previous page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRequest {
    /// Maximum number of items (implementations cap it).
    pub limit: u32,
    /// Continuation token.
    pub after: Option<String>,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            limit: 100,
            after: None,
        }
    }
}

/// One page of results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    /// Items of this page.
    pub items: Vec<T>,
    /// Token of the next page, `None` on the last page.
    pub next: Option<String>,
}

/// Repositories.
#[async_trait]
pub trait RepositoryRepo: Send + Sync {
    /// Inserts. `Conflict` when the name exists.
    async fn create(&self, repository: &Repository) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: RepositoryId) -> Result<Option<Repository>, StoreError>;
    /// Finds by name.
    async fn get_by_name(&self, name: &RepoName) -> Result<Option<Repository>, StoreError>;
    /// Lists all repositories ordered by name.
    async fn list(&self) -> Result<Vec<Repository>, StoreError>;
    /// Replaces mutable fields. `NotFound` when absent.
    async fn update(&self, repository: &Repository) -> Result<(), StoreError>;
    /// Deletes the repository and its component and asset rows. Blobs are left for GC.
    async fn delete(&self, id: RepositoryId) -> Result<(), StoreError>;
}

/// Blob store definitions.
#[async_trait]
pub trait BlobStoreRepo: Send + Sync {
    /// Inserts. `Conflict` when the name exists.
    async fn create(&self, store: &BlobStoreDef) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: BlobStoreId) -> Result<Option<BlobStoreDef>, StoreError>;
    /// Finds by name.
    async fn get_by_name(&self, name: &str) -> Result<Option<BlobStoreDef>, StoreError>;
    /// Lists all.
    async fn list(&self) -> Result<Vec<BlobStoreDef>, StoreError>;
    /// Deletes. `Conflict` when a repository still uses it.
    async fn delete(&self, id: BlobStoreId) -> Result<(), StoreError>;
}

/// Blob metadata and garbage collection support.
#[async_trait]
pub trait BlobRepo: Send + Sync {
    /// Finds by id.
    async fn get(&self, id: BlobId) -> Result<Option<BlobRecord>, StoreError>;
    /// Sum of `size_bytes` over distinct blobs referenced by assets of the repository.
    async fn used_bytes_by_repository(&self, id: RepositoryId) -> Result<u64, StoreError>;
    /// Sum of `size_bytes` of all blobs in the store.
    async fn used_bytes_by_store(&self, id: BlobStoreId) -> Result<u64, StoreError>;
    /// Blobs not referenced by any asset and older than `older_than`, oldest first.
    async fn list_unreferenced(
        &self,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError>;
    /// Deletes the row. The caller deletes the object in the store first.
    async fn delete(&self, id: BlobId) -> Result<(), StoreError>;
}

/// Filter for component listings.
#[derive(Debug, Clone, Default)]
pub struct ComponentFilter {
    /// Only this repository.
    pub repository_id: Option<RepositoryId>,
    /// Exact namespace.
    pub group_id: Option<String>,
    /// Exact name.
    pub name: Option<String>,
    /// Exact version.
    pub version: Option<String>,
    /// Full text query over group, name and version.
    pub query: Option<String>,
}

/// Components.
#[async_trait]
pub trait ComponentRepo: Send + Sync {
    /// Returns the component with that coordinate, creating it when absent.
    /// Concurrent calls for one coordinate return the same row.
    async fn find_or_create(&self, new: &NewComponent) -> Result<Component, StoreError>;
    /// Finds by id.
    async fn get(&self, id: ComponentId) -> Result<Option<Component>, StoreError>;
    /// Finds by exact coordinate.
    async fn find(&self, new: &NewComponent) -> Result<Option<Component>, StoreError>;
    /// Lists with keyset pagination.
    async fn list(
        &self,
        filter: &ComponentFilter,
        page: &PageRequest,
    ) -> Result<Page<Component>, StoreError>;
    /// Deletes the component and its assets. Blobs are left for GC.
    async fn delete(&self, id: ComponentId) -> Result<(), StoreError>;
}

/// Assets.
#[async_trait]
pub trait AssetRepo: Send + Sync {
    /// Finds by `(repository, path)`.
    async fn get_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<Asset>, StoreError>;
    /// Finds by id.
    async fn get(&self, id: AssetId) -> Result<Option<Asset>, StoreError>;
    /// Inserts the blob row and creates or switches the asset to it in one
    /// transaction. The previous blob, if any, is returned for GC. This single
    /// commit is what makes an upload visible.
    async fn publish(&self, new: &NewAsset) -> Result<PublishOutcome, StoreError>;
    /// Lists assets of a repository whose path starts with `prefix`.
    async fn list(
        &self,
        repository_id: RepositoryId,
        prefix: &str,
        page: &PageRequest,
    ) -> Result<Page<Asset>, StoreError>;
    /// Lists assets of a component.
    async fn list_by_component(&self, id: ComponentId) -> Result<Vec<Asset>, StoreError>;
    /// Deletes by path and returns the blob that lost its reference.
    async fn delete_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<BlobRecord>, StoreError>;
    /// Records downloads in one batch: `(asset, time)`.
    async fn touch_downloaded(&self, batch: &[(AssetId, OffsetDateTime)])
    -> Result<(), StoreError>;
}

/// Users and their role assignments.
#[async_trait]
pub trait UserRepo: Send + Sync {
    /// Finds by id.
    async fn get(&self, id: UserId) -> Result<Option<User>, StoreError>;
    /// Finds by username across all sources (usernames are globally unique).
    async fn find_by_username(&self, username: &str) -> Result<Option<User>, StoreError>;
    /// Finds an external account by `(source, issuer, subject)`. Never by email or username.
    async fn find_external(
        &self,
        source: UserSource,
        issuer: Option<&str>,
        subject: Option<&str>,
    ) -> Result<Option<User>, StoreError>;
    /// Inserts. `Conflict` on a duplicate username or external identity.
    async fn create(&self, new: &NewUser) -> Result<User, StoreError>;
    /// Lists users with keyset pagination.
    async fn list(&self, page: &PageRequest) -> Result<Page<User>, StoreError>;
    /// Replaces the password hash (rehash on login, password change).
    async fn set_password_hash(&self, id: UserId, hash: &str) -> Result<(), StoreError>;
    /// Enables or disables the account.
    async fn set_enabled(&self, id: UserId, enabled: bool) -> Result<(), StoreError>;
    /// Sets `tokens_valid_after` to `at`, revoking internal JWTs issued earlier.
    async fn set_tokens_valid_after(
        &self,
        id: UserId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError>;
    /// Roles of the user with their origin.
    async fn roles_of(&self, id: UserId) -> Result<Vec<(RoleRef, RoleOrigin)>, StoreError>;
    /// Replaces only the rows of `origin` with `roles` in one transaction and
    /// notifies other nodes. Rows of other origins are untouched.
    async fn replace_roles_for_origin(
        &self,
        id: UserId,
        origin: RoleOrigin,
        roles: &[RoleId],
    ) -> Result<(), StoreError>;
    /// Deletes the account and its tokens.
    async fn delete(&self, id: UserId) -> Result<(), StoreError>;
}

/// Roles.
#[async_trait]
pub trait RoleRepo: Send + Sync {
    /// Finds by id.
    async fn get(&self, id: RoleId) -> Result<Option<Role>, StoreError>;
    /// Finds by name.
    async fn get_by_name(&self, name: &str) -> Result<Option<Role>, StoreError>;
    /// Lists all.
    async fn list(&self) -> Result<Vec<Role>, StoreError>;
    /// Inserts. `Conflict` on duplicate name or a nesting cycle.
    async fn create(&self, role: &Role) -> Result<(), StoreError>;
    /// Replaces a non built-in role. `Conflict` on a nesting cycle.
    async fn update(&self, role: &Role) -> Result<(), StoreError>;
    /// Deletes a non built-in role.
    async fn delete(&self, id: RoleId) -> Result<(), StoreError>;
    /// All privileges reachable from `roles` through nesting (recursive, cycle safe).
    async fn effective_privileges(&self, roles: &[RoleId]) -> Result<Vec<Privilege>, StoreError>;
}

/// Privileges.
#[async_trait]
pub trait PrivilegeRepo: Send + Sync {
    /// Finds by id.
    async fn get(&self, id: PrivilegeId) -> Result<Option<Privilege>, StoreError>;
    /// Lists all.
    async fn list(&self) -> Result<Vec<Privilege>, StoreError>;
    /// Inserts.
    async fn create(&self, privilege: &Privilege) -> Result<(), StoreError>;
    /// Replaces a non built-in privilege.
    async fn update(&self, privilege: &Privilege) -> Result<(), StoreError>;
    /// Deletes a non built-in privilege.
    async fn delete(&self, id: PrivilegeId) -> Result<(), StoreError>;
}

/// Content selectors.
#[async_trait]
pub trait SelectorRepo: Send + Sync {
    /// Finds by id.
    async fn get(&self, id: ContentSelectorId) -> Result<Option<ContentSelector>, StoreError>;
    /// Lists all.
    async fn list(&self) -> Result<Vec<ContentSelector>, StoreError>;
    /// Inserts.
    async fn create(&self, selector: &ContentSelector) -> Result<(), StoreError>;
    /// Replaces.
    async fn update(&self, selector: &ContentSelector) -> Result<(), StoreError>;
    /// Deletes. `Conflict` while a privilege references it.
    async fn delete(&self, id: ContentSelectorId) -> Result<(), StoreError>;
}

/// API tokens.
#[async_trait]
pub trait TokenRepo: Send + Sync {
    /// Inserts a token record (hash only).
    async fn create(&self, token: &ApiToken) -> Result<(), StoreError>;
    /// Looks a token up by the SHA-256 hex of the presented string (indexed).
    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<ApiToken>, StoreError>;
    /// Tokens of a user.
    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<ApiToken>, StoreError>;
    /// Marks revoked.
    async fn revoke(&self, id: ApiTokenId, at: OffsetDateTime) -> Result<(), StoreError>;
    /// Updates `last_used_at` (implementations may coalesce writes).
    async fn touch_last_used(&self, id: ApiTokenId, at: OffsetDateTime) -> Result<(), StoreError>;
}

/// Group to role mappings.
#[async_trait]
pub trait IdpMappingRepo: Send + Sync {
    /// Mappings of one provider ordered by priority.
    async fn list(&self, provider: IdpProvider) -> Result<Vec<IdpGroupMapping>, StoreError>;
    /// Inserts or replaces by `(provider, external_group)`.
    async fn upsert(&self, mapping: &IdpGroupMapping) -> Result<(), StoreError>;
    /// Deletes.
    async fn delete(&self, id: IdpGroupMappingId) -> Result<(), StoreError>;
}

/// Subject of the failed-login counter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoginKey {
    /// Account name as typed (case folded by the caller).
    pub account: String,
    /// Source address for the `(account, ip)` counter. `None` selects the per-account counter.
    pub ip: Option<IpAddr>,
}

/// State of one failed-login counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginState {
    /// Consecutive failures since the last success or reset.
    pub failures: u32,
    /// Time of the last failure.
    pub last_failure_at: Option<OffsetDateTime>,
}

/// Failed-login counters. PostgreSQL is the source of truth, shared by all nodes
/// and threads. Redis may cache but is never the only copy.
#[async_trait]
pub trait LoginAttemptRepo: Send + Sync {
    /// Atomically increments the counter and returns the new state.
    async fn register_failure(
        &self,
        key: &LoginKey,
        at: OffsetDateTime,
    ) -> Result<LoginState, StoreError>;
    /// Reads the counter.
    async fn state(&self, key: &LoginKey) -> Result<LoginState, StoreError>;
    /// Clears the counter after a successful login.
    async fn reset(&self, key: &LoginKey) -> Result<(), StoreError>;
}
