//! Repositories, blob store definitions and blob rows.

use std::collections::HashSet;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{BlobId, BlobStoreId, RepositoryId};
use rustryx_core::model::{BlobRecord, BlobStoreDef, Repository};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{BlobRepo, BlobStoreRepo, RepositoryRepo};
use time::OffsetDateTime;

use super::{MemStore, conflict, not_found};

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
        state.repositories.insert(repository.id, repository.clone());
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
        state.repositories.insert(next.id, next);
        Ok(())
    }

    async fn delete(&self, id: RepositoryId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.repositories.remove(&id).is_none() {
            return Err(not_found("repository", id));
        }
        state.components.retain(|_, c| c.repository_id != id);
        let doomed: Vec<_> = state
            .assets
            .values()
            .filter(|a| a.repository_id == id)
            .map(|a| a.id)
            .collect();
        for asset_id in doomed {
            state.remove_asset(asset_id);
        }
        // Blob rows stay on purpose: the port leaves them for garbage collection.
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

    async fn delete(&self, id: BlobStoreId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.blob_stores.contains_key(&id) {
            return Err(not_found("blob store", id));
        }
        if state.repositories.values().any(|r| r.blob_store_id == id) {
            return Err(conflict("blob store is still used by a repository"));
        }
        state.blob_stores.remove(&id);
        Ok(())
    }
}

#[async_trait]
impl BlobRepo for MemStore {
    async fn get(&self, id: BlobId) -> Result<Option<BlobRecord>, StoreError> {
        Ok(self.tables()?.blobs.get(&id).cloned())
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
            .filter(|b| !referenced.contains(&b.id) && b.created_at < older_than)
            .cloned()
            .collect();
        candidates.sort_by_key(|b| (b.created_at, b.id));
        candidates.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(candidates)
    }

    async fn delete(&self, id: BlobId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.blobs.contains_key(&id) {
            return Err(not_found("blob", id));
        }
        // PostgreSQL would refuse through the foreign key; deleting the row of a blob an
        // asset still serves would turn a later download into a dangling reference.
        if state.assets.values().any(|a| a.blob_id == id) {
            return Err(conflict("blob is still referenced by an asset"));
        }
        state.blobs.remove(&id);
        Ok(())
    }
}
