//! Metadata store ports (implemented by `rustryx-db` on PostgreSQL).
//!
//! All methods are cancel safe: each is one statement or one transaction, so a
//! cancelled future leaves either the old or the new state, never a mix.
//! Errors are [`StoreError`]: `NotFound`, `Conflict` (unique violation),
//! `Invalid` (malformed input such as a bad page token), `Unavailable`, `Backend`.
//! Methods that enforce policies inside their transaction return [`MutationError`].
//!
//! Absent target semantics, the contract that `rustryx-db` and the in memory double of
//! `rustryx-testkit` both follow and the conformance suite checks:
//!
//! - Admin facing entities (repository, blob store, user, role, privilege, selector, mapping,
//!   component, token): `update`, `delete`, `set_*` and `revoke` of a row that does not exist
//!   is `NotFound`.
//! - GC facing calls and counters are idempotent: `BlobRepo::delete` and the `touch_*` family
//!   succeed on a missing row, `AssetRepo::delete_by_path` and `delete_by_path_checked` return
//!   `None` for a missing path, `BlobRepo::delete_if_unreferenced` returns `Ok(false)`, and
//!   `AssetRepo::record_downloads` ignores an asset that no longer exists.
//! - Deleting a role removes the rows that link it (role to privilege, role to role, user to
//!   role); deleting a privilege removes its links to roles. A group mapping that names the
//!   role is a `Conflict`, as is a selector that a privilege references.

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{MutationError, StoreError};
use crate::identity::{
    ApiToken, ContentSelector, IdpGroupMapping, IdpProvider, NewUser, Privilege, Role, RoleOrigin,
    RoleRef, User, UserSource,
};
use crate::ids::{
    ApiTokenId, AssetId, BlobId, BlobStoreId, ComponentId, ContentSelectorId, IdpGroupMappingId,
    PrivilegeId, RepositoryId, RoleId, UserId,
};
use crate::model::{
    Asset, BlobRecord, BlobStoreDef, Component, DownloadTick, NewAsset, NewAssetLink, NewComponent,
    PublishOutcome, Repository, WriteOptions,
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
    /// Replaces the editable fields (config, quotas, group settings). `NotFound` when absent.
    /// The registry cache is told through the invalidation bus.
    async fn update(&self, store: &BlobStoreDef) -> Result<(), StoreError>;
    /// Deletes. `Conflict` when a repository still uses it.
    async fn delete(&self, id: BlobStoreId) -> Result<(), StoreError>;
}

/// Blob metadata and garbage collection support.
#[async_trait]
pub trait BlobRepo: Send + Sync {
    /// Finds by id.
    async fn get(&self, id: BlobId) -> Result<Option<BlobRecord>, StoreError>;
    /// Inserts a blob row outside a publish (tests and the GC reconciliation seed rows through
    /// the port). `Conflict` when the id or the `(store, key)` pair exists.
    async fn insert(&self, record: &BlobRecord) -> Result<(), StoreError>;
    /// Sum of `size_bytes` over distinct blobs referenced by assets of the repository.
    async fn used_bytes_by_repository(&self, id: RepositoryId) -> Result<u64, StoreError>;
    /// Sum of `size_bytes` of all blobs in the store (a group resolves to its members).
    async fn used_bytes_by_store(&self, id: BlobStoreId) -> Result<u64, StoreError>;
    /// How many blob rows the store has. The default is zero for a store that does not count.
    async fn count_by_store(&self, id: BlobStoreId) -> Result<u64, StoreError> {
        let _ = id;
        Ok(0)
    }
    /// Blobs not referenced by any asset and older than `older_than`, oldest first.
    async fn list_unreferenced(
        &self,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError>;
    /// Like `list_unreferenced`, restricted to one store: the GC takes one lock per store.
    async fn list_unreferenced_in(
        &self,
        store: BlobStoreId,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError>;
    /// Deletes the row only. For tests and tooling: it is NOT safe for garbage collection,
    /// because deleting the object first can leave an asset that points at nothing once
    /// `AssetRepo::link` may reference an orphan again. The GC uses
    /// [`BlobRepo::delete_if_unreferenced`]. Idempotent.
    async fn delete(&self, id: BlobId) -> Result<(), StoreError>;
    /// The GC sweep step, in one transaction: lock the blob row `FOR UPDATE`, re-check that
    /// `orphaned_at` is older than `older_than` and that no asset references the blob, delete
    /// the ROW and commit. `Ok(false)` when the blob gained a reference or is too young (it is
    /// kept) or does not exist. The caller deletes the OBJECT only after `Ok(true)`: row
    /// first, object second, so a reference created through `link` after the mark always wins
    /// and an asset can never point at a deleted object. A crash between the commit and the
    /// object delete leaves an orphan object, which `verify_blobs` finds. Cancel safe.
    async fn delete_if_unreferenced(
        &self,
        id: BlobId,
        older_than: OffsetDateTime,
    ) -> Result<bool, StoreError>;
    /// Switches a blob to another store with compare and set on the current store. `false`
    /// when it already moved (or does not exist). Provided: the default moves nothing and
    /// answers `Ok(false)`, which is safe; implementers of the storage stream override it.
    async fn move_blob(
        &self,
        _id: BlobId,
        _from: BlobStoreId,
        _to: BlobStoreId,
    ) -> Result<bool, StoreError> {
        Ok(false)
    }
    /// Rows of a store after `after`, in id order, for the move task and for verification.
    /// Referenced rows only when `referenced_only`. Provided: the default fails with
    /// `StoreError::Invalid`, never with an empty page, because verification would read an
    /// empty page as "the store has no rows" and report every object as an orphan.
    async fn list_page_by_store(
        &self,
        _store: BlobStoreId,
        _after: Option<BlobId>,
        _limit: u32,
        _referenced_only: bool,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        Err(StoreError::Invalid(
            "list_page_by_store is not supported by this store".to_owned(),
        ))
    }
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
    /// Deletes the component and its assets. Blobs are left for GC. The SQL guards (immutable,
    /// hold, read only) apply: a violation surfaces as `StoreError::Conflict`.
    async fn delete(&self, id: ComponentId) -> Result<(), StoreError>;
    /// Deletes with the same guards and a typed answer: `PolicyViolation` for an immutable,
    /// held or read only refusal, with the outbox events of `options` in the same
    /// transaction. `NotFound` when absent. Cancel safe (one transaction).
    async fn delete_checked(
        &self,
        id: ComponentId,
        options: &WriteOptions,
    ) -> Result<(), MutationError>;
}

/// Assets.
///
/// The old state changing methods stay callable on purpose: the SQL guards (immutable, held,
/// quota, read only) apply to them too, so a forgotten typed call can never bypass them. The
/// typed variants (`publish_with`, `link`, `delete_by_path_checked`) only add a precise error
/// and the outbox events.
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
    /// The enforcing publish: write policy, immutability, hold, quota (before commit, on the
    /// actual size), read only mode and the outbox events all run in the same transaction as
    /// the asset switch, which inserts the blob row and creates or switches the asset to it.
    /// The previous blob, if any, is returned for GC. A refusal leaves nothing behind (no
    /// asset, no blob row, no outbox row). Cancel safe (one transaction).
    async fn publish_with(
        &self,
        new: &NewAsset,
        options: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError>;
    /// Provided: `publish_with(new, &WriteOptions::new())` with a violation flattened to
    /// `StoreError::Conflict`. Kept so phase 1 code compiles; new code calls `publish_with`.
    async fn publish(&self, new: &NewAsset) -> Result<PublishOutcome, StoreError> {
        self.publish_with(new, &WriteOptions::new())
            .await
            .map_err(MutationError::into_store_error)
    }
    /// Publishes by reference to an existing blob (promotion, mount, cross repository copy),
    /// with the same guards as `publish_with`. Both repositories must use the same blob store,
    /// otherwise `StoreError::Conflict` and the caller copies bytes. Usage is counted once per
    /// scope. Cancel safe.
    async fn link(
        &self,
        link: &NewAssetLink,
        options: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError>;
    /// Lists assets of a repository whose path starts with `prefix`.
    async fn list(
        &self,
        repository_id: RepositoryId,
        prefix: &str,
        page: &PageRequest,
    ) -> Result<Page<Asset>, StoreError>;
    /// Lists assets of a component.
    async fn list_by_component(&self, id: ComponentId) -> Result<Vec<Asset>, StoreError>;
    /// Deletes by path and returns the blob that lost its reference. The SQL guards apply: a
    /// violation surfaces as `StoreError::Conflict`.
    async fn delete_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<BlobRecord>, StoreError>;
    /// Deletes with the same guards and a typed answer; returns the blob that lost its last
    /// reference, `None` for a missing path. Cancel safe.
    async fn delete_by_path_checked(
        &self,
        repository_id: RepositoryId,
        path: &str,
        options: &WriteOptions,
    ) -> Result<Option<BlobRecord>, MutationError>;
    /// Records downloads in one batch: `(asset, time)`.
    async fn touch_downloaded(&self, batch: &[(AssetId, OffsetDateTime)])
    -> Result<(), StoreError>;
    /// Moves `updated_at` forward after an upstream revalidation (a 304), without touching the
    /// blob: an expired cache entry then costs a conditional request, not a full refetch.
    /// `NotFound` when absent.
    async fn touch_fresh(&self, id: AssetId, at: OffsetDateTime) -> Result<(), StoreError>;
    /// Batched downloads: sets `last_downloaded_at` (only when older than the configured
    /// interval) and adds the counts to the daily counters. Idempotence per batch is NOT
    /// promised: a retry may count twice, which is the accepted precision of a statistic. An
    /// unknown asset is ignored.
    async fn record_downloads(&self, batch: &[DownloadTick]) -> Result<(), StoreError>;
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
    /// Compare and set: replaces the hash only while it is still `old_hash`. `true` when
    /// swapped, `false` when someone changed it first (a concurrent rehash or a password
    /// change then wins). `NotFound` when the user is absent.
    async fn set_password_hash_if(
        &self,
        id: UserId,
        old_hash: &str,
        new_hash: &str,
    ) -> Result<bool, StoreError>;
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
    /// Tokens that expire within `within` of `now` and have no expiry notice yet, for the
    /// `token_expiry_notice` job. Provided with an empty answer; `rustryx-db` overrides it.
    async fn list_expiring(
        &self,
        _within: Duration,
        _now: OffsetDateTime,
        _limit: u32,
    ) -> Result<Vec<ApiToken>, StoreError> {
        Ok(Vec::new())
    }
    /// Marks the expiry notice as sent. Provided as a no-op; `rustryx-db` overrides it.
    async fn mark_expiry_notified(
        &self,
        _id: ApiTokenId,
        _at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        Ok(())
    }
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

#[cfg(test)]
mod tests;
