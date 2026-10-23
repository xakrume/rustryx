//! Components and assets.

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{AssetId, ComponentId, RepositoryId};
use rustryx_core::model::{Asset, BlobRecord, Component, NewAsset, NewComponent, PublishOutcome};
use rustryx_core::ports::{AssetRepo, ComponentFilter, ComponentRepo, Page, PageRequest};
use time::OffsetDateTime;

use super::paging::paginate;
use super::{MemStore, conflict, not_found};

fn same_coordinate(component: &Component, new: &NewComponent) -> bool {
    component.repository_id == new.repository_id
        && component.format == new.format
        && component.group_id == new.group_id
        && component.name == new.name
        && component.version == new.version
}

/// Every whitespace separated term must occur, case-insensitively, in the group, name
/// or version. An empty query matches everything.
fn matches_query(component: &Component, query: &str) -> bool {
    let haystack = format!(
        "{} {} {}",
        component.group_id.as_deref().unwrap_or_default(),
        component.name,
        component.version.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|term| haystack.contains(&term.to_lowercase()))
}

fn matches_filter(component: &Component, filter: &ComponentFilter) -> bool {
    filter
        .repository_id
        .is_none_or(|id| component.repository_id == id)
        && filter
            .group_id
            .as_ref()
            .is_none_or(|g| component.group_id.as_ref() == Some(g))
        && filter.name.as_ref().is_none_or(|n| &component.name == n)
        && filter
            .version
            .as_ref()
            .is_none_or(|v| component.version.as_ref() == Some(v))
        && filter
            .query
            .as_ref()
            .is_none_or(|q| matches_query(component, q))
}

#[async_trait]
impl ComponentRepo for MemStore {
    async fn find_or_create(&self, new: &NewComponent) -> Result<Component, StoreError> {
        let now = self.now();
        // Find and insert happen under one lock, which is what makes concurrent calls
        // for one coordinate return the same row.
        let mut state = self.tables()?;
        if let Some(existing) = state.components.values().find(|c| same_coordinate(c, new)) {
            return Ok(existing.clone());
        }
        let component = Component {
            id: ComponentId::generate(),
            repository_id: new.repository_id,
            format: new.format,
            group_id: new.group_id.clone(),
            name: new.name.clone(),
            version: new.version.clone(),
            created_at: now,
            updated_at: now,
        };
        state.components.insert(component.id, component.clone());
        Ok(component)
    }

    async fn get(&self, id: ComponentId) -> Result<Option<Component>, StoreError> {
        Ok(self.tables()?.components.get(&id).cloned())
    }

    async fn find(&self, new: &NewComponent) -> Result<Option<Component>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .components
            .values()
            .find(|c| same_coordinate(c, new))
            .cloned())
    }

    async fn list(
        &self,
        filter: &ComponentFilter,
        page: &PageRequest,
    ) -> Result<Page<Component>, StoreError> {
        let rows: Vec<(String, Component)> = self
            .tables()?
            .components
            .values()
            .filter(|c| matches_filter(c, filter))
            .map(|c| (c.id.to_string(), c.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn delete(&self, id: ComponentId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.components.remove(&id).is_none() {
            return Err(not_found("component", id));
        }
        let doomed: Vec<AssetId> = state
            .assets
            .values()
            .filter(|a| a.component_id == Some(id))
            .map(|a| a.id)
            .collect();
        for asset_id in doomed {
            state.remove_asset(asset_id);
        }
        Ok(())
    }
}

#[async_trait]
impl AssetRepo for MemStore {
    async fn get_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<Asset>, StoreError> {
        let state = self.tables()?;
        let id = state.asset_index.get(&(repository_id, path.to_owned()));
        Ok(id.and_then(|id| state.assets.get(id)).cloned())
    }

    async fn get(&self, id: AssetId) -> Result<Option<Asset>, StoreError> {
        Ok(self.tables()?.assets.get(&id).cloned())
    }

    async fn publish(&self, new: &NewAsset) -> Result<PublishOutcome, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        // Every check runs before the first write: a failed publish changes nothing.
        if state.blobs.contains_key(&new.blob.id) {
            return Err(conflict(format!("blob {} already exists", new.blob.id)));
        }
        let key = (new.repository_id, new.path.clone());
        let previous = state
            .asset_index
            .get(&key)
            .and_then(|id| state.assets.get(id))
            .cloned();
        let replaced: Option<BlobRecord> = previous
            .as_ref()
            .and_then(|old| state.blobs.get(&old.blob_id))
            .cloned();
        let asset = Asset {
            id: previous
                .as_ref()
                .map_or_else(AssetId::generate, |old| old.id),
            repository_id: new.repository_id,
            component_id: new.component_id,
            path: new.path.clone(),
            blob_id: new.blob.id,
            content_type: new.content_type.clone(),
            size_bytes: new.blob.size_bytes,
            checksums: new.blob.checksums.clone(),
            last_downloaded_at: previous.as_ref().and_then(|old| old.last_downloaded_at),
            created_at: previous.as_ref().map_or(now, |old| old.created_at),
            updated_at: now,
        };
        state.blobs.insert(new.blob.id, new.blob.clone());
        state.asset_index.insert(key, asset.id);
        state.assets.insert(asset.id, asset.clone());
        Ok(PublishOutcome { asset, replaced })
    }

    async fn list(
        &self,
        repository_id: RepositoryId,
        prefix: &str,
        page: &PageRequest,
    ) -> Result<Page<Asset>, StoreError> {
        let state = self.tables()?;
        let start = (repository_id, prefix.to_owned());
        let rows: Vec<(String, Asset)> = state
            .asset_index
            .range(start..)
            .take_while(|((repo, path), _)| *repo == repository_id && path.starts_with(prefix))
            .filter_map(|((_, path), id)| state.assets.get(id).map(|a| (path.clone(), a.clone())))
            .collect();
        drop(state);
        paginate(rows, page)
    }

    async fn list_by_component(&self, id: ComponentId) -> Result<Vec<Asset>, StoreError> {
        let state = self.tables()?;
        let mut assets: Vec<Asset> = state
            .assets
            .values()
            .filter(|a| a.component_id == Some(id))
            .cloned()
            .collect();
        assets.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(assets)
    }

    async fn delete_by_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<Option<BlobRecord>, StoreError> {
        let mut state = self.tables()?;
        let Some(id) = state
            .asset_index
            .get(&(repository_id, path.to_owned()))
            .copied()
        else {
            return Ok(None);
        };
        let removed = state.remove_asset(id);
        Ok(removed.and_then(|asset| state.blobs.get(&asset.blob_id).cloned()))
    }

    async fn touch_downloaded(
        &self,
        batch: &[(AssetId, OffsetDateTime)],
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        for (id, at) in batch {
            // An asset deleted between the download and the flush is not an error.
            if let Some(asset) = state.assets.get_mut(id) {
                // Batches may arrive out of order; the stamp only moves forward.
                asset.last_downloaded_at =
                    Some(asset.last_downloaded_at.map_or(*at, |old| old.max(*at)));
            }
        }
        Ok(())
    }
}
