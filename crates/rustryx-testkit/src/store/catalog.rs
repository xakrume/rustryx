//! Repositories, blob store definitions and blob rows.

use std::collections::HashSet;

use async_trait::async_trait;
use rustryx_core::error::{MutationError, PolicyViolation, StoreError};
use rustryx_core::ids::{BlobId, BlobStoreId, RepositoryId};
use rustryx_core::model::{BlobRecord, BlobStoreDef, ProjectRef, Repository};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{BlobRepo, BlobStoreRepo, InvalidationEvent, RepositoryRepo};
use rustryx_core::project::ProjectVisibility;
use time::OffsetDateTime;

use super::state::State;
use super::{MemStore, conflict, not_found};

impl State {
    /// The `repositories_project_sync` trigger: a repository in a project is public exactly
    /// when the project is, whatever the caller wrote, and carries the project as it is now.
    /// A repository outside any project keeps the flag it was given.
    fn sync_project(&self, repository: &mut Repository) -> Result<(), StoreError> {
        let Some(reference) = repository.project.clone() else {
            return Ok(());
        };
        // The foreign key on `repositories.project_id`, which the database reports as a
        // conflict like every other constraint.
        let project = self
            .projects
            .get(&reference.id)
            .ok_or_else(|| conflict(format!("project {} does not exist", reference.id)))?;
        repository.project = Some(ProjectRef {
            id: project.id,
            name: project.name.clone(),
            visibility: project.visibility,
        });
        repository.allow_anonymous = project.visibility == ProjectVisibility::Public;
        Ok(())
    }

    /// The guards that fire when a repository delete cascades: `assets_readonly` and
    /// `components_readonly` (statement level, so they fire even for a repository without
    /// content), then the component and asset guards for every row that would go.
    fn guard_repository_delete(
        &self,
        repository: &Repository,
        now: OffsetDateTime,
    ) -> Result<(), PolicyViolation> {
        self.check_writable()?;
        for component in self
            .components
            .values()
            .filter(|c| c.repository_id == repository.id)
        {
            self.guard_component_delete(Some(repository), component, false, now)?;
        }
        for asset in self
            .assets
            .values()
            .filter(|a| a.repository_id == repository.id)
        {
            self.guard_asset_change(repository, asset, false, now)?;
        }
        Ok(())
    }
}

#[async_trait]
impl RepositoryRepo for MemStore {
    async fn create(&self, repository: &Repository) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let taken = state
            .repositories
            .values()
            .any(|r| r.id == repository.id || r.name == repository.name);
        if taken {
            return Err(conflict(format!(
                "repository {} already exists",
                repository.name
            )));
        }
        let mut stored = repository.clone();
        state.sync_project(&mut stored)?;
        state.repositories.insert(stored.id, stored);
        Ok(())
    }

    async fn get(&self, id: RepositoryId) -> Result<Option<Repository>, StoreError> {
        Ok(self.tables()?.repositories.get(&id).cloned())
    }

    async fn get_by_name(&self, name: &RepoName) -> Result<Option<Repository>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .repositories
            .values()
            .find(|r| &r.name == name)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<Repository>, StoreError> {
        let mut all: Vec<Repository> = self.tables()?.repositories.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn update(&self, repository: &Repository) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let existing = state
            .repositories
            .get(&repository.id)
            .ok_or_else(|| not_found("repository", repository.id))?;
        // Format and type decide which engine serves the content and which config
        // sections must exist, so changing them under live content is refused loudly
        // instead of being silently ignored.
        if existing.format != repository.format || existing.repo_type != repository.repo_type {
            return Err(conflict("repository format and type are immutable"));
        }
        let created_at = existing.created_at;
        // The project link belongs to `ProjectRepo::attach_repository` and `detach_repository`:
        // an update that carried a stale or forged `project` must not move the repository.
        let project = existing.project.clone();
        let renamed_onto_other = state
            .repositories
            .values()
            .any(|r| r.id != repository.id && r.name == repository.name);
        if renamed_onto_other {
            return Err(conflict(format!(
                "repository {} already exists",
                repository.name
            )));
        }
        let mut next = repository.clone();
        next.created_at = created_at;
        next.project = project;
        state.sync_project(&mut next)?;
        state.repositories.insert(next.id, next);
        Ok(())
    }

    async fn delete(&self, id: RepositoryId) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let repository = state
            .repositories
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found("repository", id))?;
        // Immutability, holds and read only mode also stop a cascade, which is why the SQL
        // guards are triggers and not checks in the service.
        state
            .guard_repository_delete(&repository, now)
            .map_err(|violation| MutationError::from(violation).into_store_error())?;
        state.repositories.remove(&id);
        state.remove_repository_rows(id);
        Ok(())
    }
}

#[async_trait]
impl BlobStoreRepo for MemStore {
    async fn create(&self, store: &BlobStoreDef) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let taken = state
            .blob_stores
            .values()
            .any(|s| s.id == store.id || s.name == store.name);
        if taken {
            return Err(conflict(format!(
                "blob store {} already exists",
                store.name
            )));
        }
        state.blob_stores.insert(store.id, store.clone());
        Ok(())
    }

    async fn get(&self, id: BlobStoreId) -> Result<Option<BlobStoreDef>, StoreError> {
        Ok(self.tables()?.blob_stores.get(&id).cloned())
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<BlobStoreDef>, StoreError> {
        let state = self.tables()?;
        Ok(state.blob_stores.values().find(|s| s.name == name).cloned())
    }

    async fn list(&self) -> Result<Vec<BlobStoreDef>, StoreError> {
        let mut all: Vec<BlobStoreDef> = self.tables()?.blob_stores.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn update(&self, store: &BlobStoreDef) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let existing = state
            .blob_stores
            .get(&store.id)
            .ok_or_else(|| not_found("blob store", store.id))?;
        // The kind decides which adapter the registry builds and what the config means, so
        // it is fixed at creation, like the format of a repository.
        if existing.kind != store.kind {
            return Err(conflict("blob store kind is immutable"));
        }
        let name_taken = state
            .blob_stores
            .values()
            .any(|s| s.id != store.id && s.name == store.name);
        if name_taken {
            return Err(conflict(format!(
                "blob store {} already exists",
                store.name
            )));
        }
        let mut next = store.clone();
        next.created_at = existing.created_at;
        state.blob_stores.insert(next.id, next);
        drop(state);
        // There is no event for "one store changed", and the registry cache must not keep
        // an adapter built from the old config, so every consumer drops its caches.
        self.notify(InvalidationEvent::Reset);
        Ok(())
    }

    async fn delete(&self, id: BlobStoreId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.blob_stores.contains_key(&id) {
            return Err(not_found("blob store", id));
        }
        if state.repositories.values().any(|r| r.blob_store_id == id) {
            return Err(conflict("blob store is still used by a repository"));
        }
        // A member cannot be dropped from under its group, a group takes its member rows
        // with it (`ON DELETE CASCADE` on the group side only).
        if state.store_members.iter().any(|m| m.member_id == id) {
            return Err(conflict("blob store is a member of a group"));
        }
        state.store_members.retain(|m| m.group_id != id);
        state.store_stats.remove(&id);
        state.blob_stores.remove(&id);
        Ok(())
    }
}

#[async_trait]
impl BlobRepo for MemStore {
    async fn get(&self, id: BlobId) -> Result<Option<BlobRecord>, StoreError> {
        Ok(self.tables()?.blobs.get(&id).cloned())
    }

    async fn insert(&self, record: &BlobRecord) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let taken = state.blobs.values().any(|b| {
            b.id == record.id || (b.blob_store_id == record.blob_store_id && b.key == record.key)
        });
        if taken {
            return Err(conflict(format!("blob {} already exists", record.id)));
        }
        state.blobs.insert(record.id, record.clone());
        Ok(())
    }

    async fn used_bytes_by_repository(&self, id: RepositoryId) -> Result<u64, StoreError> {
        let state = self.tables()?;
        let mut counted = HashSet::new();
        let total = state
            .assets
            .values()
            .filter(|a| a.repository_id == id)
            // Distinct blobs only: one blob shared by two assets is stored once.
            .filter(|a| counted.insert(a.blob_id))
            .fold(0_u64, |sum, a| sum.saturating_add(a.size_bytes));
        Ok(total)
    }

    async fn used_bytes_by_store(&self, id: BlobStoreId) -> Result<u64, StoreError> {
        let state = self.tables()?;
        let total = state
            .blobs
            .values()
            .filter(|b| b.blob_store_id == id)
            .fold(0_u64, |sum, b| sum.saturating_add(b.size_bytes));
        Ok(total)
    }

    async fn count_by_store(&self, id: BlobStoreId) -> Result<u64, StoreError> {
        let state = self.tables()?;
        let count = state.blobs.values().filter(|b| b.blob_store_id == id).count();
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    async fn list_unreferenced(
        &self,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        let state = self.tables()?;
        let referenced: HashSet<BlobId> = state.assets.values().map(|a| a.blob_id).collect();
        let mut candidates: Vec<BlobRecord> = state
            .blobs
            .values()
            .filter(|b| !referenced.contains(&b.id) && state.blob_age_start(b) < older_than)
            .cloned()
            .collect();
        candidates.sort_by_key(|b| (state.blob_age_start(b), b.id));
        candidates.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(candidates)
    }

    async fn list_unreferenced_in(
        &self,
        store: BlobStoreId,
        older_than: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        let state = self.tables()?;
        let referenced: HashSet<BlobId> = state.assets.values().map(|a| a.blob_id).collect();
        let mut candidates: Vec<BlobRecord> = state
            .blobs
            .values()
            .filter(|b| b.blob_store_id == store)
            .filter(|b| !referenced.contains(&b.id) && state.blob_age_start(b) < older_than)
            .cloned()
            .collect();
        candidates.sort_by_key(|b| (state.blob_age_start(b), b.id));
        candidates.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(candidates)
    }

    async fn delete(&self, id: BlobId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // Idempotent like the port says: the GC path deletes after another node did.
        if !state.blobs.contains_key(&id) {
            return Ok(());
        }
        // PostgreSQL would refuse through the foreign key; deleting the row of a blob an
        // asset still serves would turn a later download into a dangling reference.
        if state.assets.values().any(|a| a.blob_id == id) {
            return Err(conflict("blob is still referenced by an asset"));
        }
        state.blobs.remove(&id);
        Ok(())
    }

    async fn move_blob(
        &self,
        id: BlobId,
        from: BlobStoreId,
        to: BlobStoreId,
    ) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        if !state.blob_stores.contains_key(&to) {
            return Err(conflict(format!("blob store {to} does not exist")));
        }
        // Compare and set on the current store: a blob that already moved (or never was in
        // `from`) answers `false`, and so does one that does not exist.
        match state.blobs.get_mut(&id) {
            Some(blob) if blob.blob_store_id == from => {
                blob.blob_store_id = to;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn list_page_by_store(
        &self,
        store: BlobStoreId,
        after: Option<BlobId>,
        limit: u32,
        referenced_only: bool,
    ) -> Result<Vec<BlobRecord>, StoreError> {
        let state = self.tables()?;
        let referenced: HashSet<BlobId> = state.assets.values().map(|a| a.blob_id).collect();
        let mut rows: Vec<BlobRecord> = state
            .blobs
            .values()
            .filter(|b| b.blob_store_id == store)
            .filter(|b| after.is_none_or(|a| b.id > a))
            .filter(|b| !referenced_only || referenced.contains(&b.id))
            .cloned()
            .collect();
        rows.sort_by_key(|b| b.id);
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(rows)
    }

    async fn delete_if_unreferenced(
        &self,
        id: BlobId,
        older_than: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        // One lock covers the age check, the reference check and the removal, which is the
        // `SELECT ... FOR UPDATE` transaction of the port. The in memory store has no
        // object to delete, so the caller's second step has nothing to race with here.
        let mut state = self.tables()?;
        let Some(blob) = state.blobs.get(&id) else {
            return Ok(false);
        };
        let referenced = state.assets.values().any(|a| a.blob_id == id);
        if referenced || state.blob_age_start(blob) >= older_than {
            return Ok(false);
        }
        state.blobs.remove(&id);
        state.orphaned_at.remove(&id);
        Ok(true)
    }
}
