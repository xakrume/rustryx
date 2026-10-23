//! Components and assets.

use async_trait::async_trait;
use rustryx_core::error::{MutationError, StoreError};
use rustryx_core::ids::{AssetId, ComponentId, RepositoryId};
use rustryx_core::model::{
    Asset, BlobRecord, Component, DownloadTick, NewAsset, NewAssetLink, NewComponent,
    PublishOutcome, WriteOptions,
};
use rustryx_core::ports::{AssetRepo, ComponentFilter, ComponentRepo, Page, PageRequest};
use time::OffsetDateTime;

use super::outbox::validate_drafts;
use super::paging::paginate;
use super::publish::{Incoming, publish_locked, settle_after_delete};
use super::{MemStore, not_found};

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
        // `components_readonly` fires on INSERT: creating a row is a write, finding one is not.
        state
            .check_writable()
            .map_err(|violation| MutationError::from(violation).into_store_error())?;
        let mut component = Component::new(
            ComponentId::generate(),
            new.repository_id,
            new.format,
            new.name.clone(),
            now,
        );
        component.group_id.clone_from(&new.group_id);
        component.version.clone_from(&new.version);
        component.created_by = new.created_by;
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
        self.delete_component(id, &WriteOptions::new())
            .map_err(MutationError::into_store_error)
    }

    async fn delete_checked(
        &self,
        id: ComponentId,
        options: &WriteOptions,
    ) -> Result<(), MutationError> {
        self.delete_component(id, options)
    }
}

impl MemStore {
    /// One transaction: guards, then the component with its assets, then the events.
    fn delete_component(
        &self,
        id: ComponentId,
        options: &WriteOptions,
    ) -> Result<(), MutationError> {
        validate_drafts(&options.events)?;
        let now = self.now();
        let mut state = self.tables()?;
        let component = state
            .components
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found("component", id))?;
        state.check_writable()?;
        let repository = state.repositories.get(&component.repository_id).cloned();
        state.guard_component_delete(
            repository.as_ref(),
            &component,
            options.override_immutable,
            now,
        )?;
        state.components.remove(&id);
        state.remove_component_rows(id);
        state.push_events(&options.events, now);
        if let Some(repository) = &repository {
            let scopes = state.scopes_of(repository);
            settle_after_delete(&mut state, &scopes);
        }
        Ok(())
    }

    /// One transaction: guards, then the asset row, then the events. Returns the blob that
    /// lost its last reference.
    fn delete_asset_at(
        &self,
        repository_id: RepositoryId,
        path: &str,
        options: &WriteOptions,
    ) -> Result<Option<BlobRecord>, MutationError> {
        validate_drafts(&options.events)?;
        let now = self.now();
        let mut state = self.tables()?;
        let Some(id) = state
            .asset_index
            .get(&(repository_id, path.to_owned()))
            .copied()
        else {
            return Ok(None);
        };
        state.check_writable()?;
        if let (Some(asset), Some(repository)) = (
            state.assets.get(&id),
            state.repositories.get(&repository_id),
        ) {
            state.guard_asset_change(repository, asset, options.override_immutable, now)?;
        }
        let Some(removed) = state.remove_asset(id) else {
            return Ok(None);
        };
        state.push_events(&options.events, now);
        if let Some(repository) = state.repositories.get(&repository_id).cloned() {
            let scopes = state.scopes_of(&repository);
            settle_after_delete(&mut state, &scopes);
        }
        let still_referenced = state.assets.values().any(|a| a.blob_id == removed.blob_id);
        Ok(if still_referenced {
            None
        } else {
            state.blobs.get(&removed.blob_id).cloned()
        })
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

    async fn publish_with(
        &self,
        new: &NewAsset,
        options: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError> {
        validate_drafts(&options.events)?;
        let now = self.now();
        let mut state = self.tables()?;
        let incoming = Incoming::from_new(new);
        publish_locked(&mut state, &incoming, options, now)
    }

    async fn link(
        &self,
        link: &NewAssetLink,
        options: &WriteOptions,
    ) -> Result<PublishOutcome, MutationError> {
        validate_drafts(&options.events)?;
        let now = self.now();
        let mut state = self.tables()?;
        let blob = state
            .blobs
            .get(&link.blob_id)
            .cloned()
            .ok_or_else(|| not_found("blob", link.blob_id))?;
        let incoming = Incoming::from_link(link, blob);
        publish_locked(&mut state, &incoming, options, now)
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
        self.delete_asset_at(repository_id, path, &WriteOptions::new())
            .map_err(MutationError::into_store_error)
    }

    async fn delete_by_path_checked(
        &self,
        repository_id: RepositoryId,
        path: &str,
        options: &WriteOptions,
    ) -> Result<Option<BlobRecord>, MutationError> {
        self.delete_asset_at(repository_id, path, options)
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

    async fn touch_fresh(&self, id: AssetId, at: OffsetDateTime) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let asset = state
            .assets
            .get_mut(&id)
            .ok_or_else(|| not_found("asset", id))?;
        // Forward only: a late 304 must not make a fresher entry look older. The blob, the
        // checksums and the guards are not involved, which is the point of the call.
        asset.updated_at = asset.updated_at.max(at);
        Ok(())
    }

    async fn record_downloads(&self, batch: &[DownloadTick]) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        for tick in batch {
            // An asset deleted between the download and the flush is ignored, as documented.
            let Some(asset) = state.assets.get_mut(&tick.asset_id) else {
                continue;
            };
            asset.last_downloaded_at = Some(
                asset
                    .last_downloaded_at
                    .map_or(tick.at, |old| old.max(tick.at)),
            );
            let component_id = asset.component_id;
            if let Some(component) = component_id.and_then(|id| state.components.get_mut(&id)) {
                component.last_downloaded_at = Some(
                    component
                        .last_downloaded_at
                        .map_or(tick.at, |old| old.max(tick.at)),
                );
            }
            // The double has no flush interval, so `last_downloaded_at` always moves; the
            // daily counter is keyed by the UTC date of the tick.
            let day = tick.at.to_offset(time::UtcOffset::UTC).date();
            let counter = state.downloads.entry((tick.asset_id, day)).or_default();
            *counter = counter.saturating_add(u64::from(tick.count));
        }
        Ok(())
    }
}
