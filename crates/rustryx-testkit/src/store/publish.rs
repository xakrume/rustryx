//! The write path shared by `publish_with` and `link`: guards, quota, the asset switch and
//! the events, as one step under the table lock.

use rustryx_core::audit::ActorRef;
use rustryx_core::error::{MutationError, PolicyViolation, StoreError};
use rustryx_core::events::{EventDraft, EventKind};
use rustryx_core::ids::{AssetId, ComponentId, UserId};
use rustryx_core::model::{
    Asset, BlobRecord, NewAsset, NewAssetLink, PublishMode, PublishOutcome, WriteOptions,
};
use rustryx_core::quota::QuotaScope;
use time::OffsetDateTime;

use super::conflict;
use super::state::State;

/// What a publish or a link puts at a path.
pub(crate) struct Incoming {
    repository_id: rustryx_core::ids::RepositoryId,
    component_id: Option<ComponentId>,
    path: String,
    content_type: String,
    blob: BlobRecord,
    created_by: Option<UserId>,
    /// `publish_with` inserts the blob row, `link` points at one that exists.
    insert_blob: bool,
}

impl Incoming {
    pub(crate) fn from_new(new: &NewAsset) -> Self {
        Self {
            repository_id: new.repository_id,
            component_id: new.component_id,
            path: new.path.clone(),
            content_type: new.content_type.clone(),
            blob: new.blob.clone(),
            created_by: new.created_by,
            insert_blob: true,
        }
    }

    pub(crate) fn from_link(link: &NewAssetLink, blob: BlobRecord) -> Self {
        Self {
            repository_id: link.repository_id,
            component_id: link.component_id,
            path: link.path.clone(),
            content_type: link.content_type.clone(),
            blob,
            created_by: link.created_by,
            insert_blob: false,
        }
    }
}

/// Runs the whole write against `state`. Every check comes before the first mutation, so a
/// refusal changes nothing: no asset, no blob row, no outbox row.
///
/// Order of the refusals: blob row conflict, read only mode, write policy, create only,
/// immutability, hold, quota. The database fires the first four of the trigger guards in
/// table trigger order and the others in Rust before the asset switch; a test that triggers
/// two at once learns this order, nothing else promises it.
pub(crate) fn publish_locked(
    state: &mut State,
    incoming: &Incoming,
    options: &WriteOptions,
    now: OffsetDateTime,
) -> Result<PublishOutcome, MutationError> {
    let repository = state.repositories.get(&incoming.repository_id).cloned();
    if incoming.insert_blob {
        check_blob_free(state, &incoming.blob)?;
    } else if let Some(repository) = &repository {
        // The target store must be able to serve the blob: the same store, or a group that
        // lists the blob's store as a member. Anything else needs the bytes copied.
        if !state.store_serves(repository.blob_store_id, incoming.blob.blob_store_id) {
            return Err(conflict("link needs both repositories to use the same blob store").into());
        }
    }
    state.check_writable()?;
    let key = (incoming.repository_id, incoming.path.clone());
    let previous = state
        .asset_index
        .get(&key)
        .and_then(|id| state.assets.get(id))
        .cloned();
    if let Some(repository) = &repository {
        State::check_write_policy(repository, previous.as_ref())?;
    }
    if previous.is_some() && options.mode == PublishMode::CreateOnly {
        return Err(PolicyViolation::AlreadyExists.into());
    }
    if let (Some(repository), Some(old)) = (&repository, &previous) {
        // An overwrite with the same SHA-256 is an idempotent re-push (ADR-W2-10) and
        // changes no content, so neither immutability nor a hold applies to it.
        if old.checksums.sha256 != incoming.blob.checksums.sha256 {
            state.guard_asset_change(repository, old, options.override_immutable, now)?;
        }
    }
    if let Some(repository) = &repository {
        state.check_quota(repository, &incoming.blob, previous.as_ref())?;
    }
    Ok(commit(
        state,
        incoming,
        options,
        now,
        repository.as_ref(),
        previous.as_ref(),
    ))
}

fn check_blob_free(state: &State, blob: &BlobRecord) -> Result<(), StoreError> {
    if state.blobs.contains_key(&blob.id) {
        return Err(conflict(format!("blob {} already exists", blob.id)));
    }
    let key_taken = state
        .blobs
        .values()
        .any(|b| b.blob_store_id == blob.blob_store_id && b.key == blob.key);
    if key_taken {
        return Err(conflict("a blob with that store and key already exists"));
    }
    Ok(())
}

fn commit(
    state: &mut State,
    incoming: &Incoming,
    options: &WriteOptions,
    now: OffsetDateTime,
    repository: Option<&rustryx_core::model::Repository>,
    previous: Option<&Asset>,
) -> PublishOutcome {
    let scopes_before: Vec<(QuotaScope, u64)> = repository
        .map(|r| state.scopes_of(r))
        .unwrap_or_default()
        .into_iter()
        .map(|scope| (scope, state.scope_usage(scope).0))
        .collect();
    let id = previous.map_or_else(AssetId::generate, |old| old.id);
    let mut asset = Asset::from_blob(
        id,
        incoming.repository_id,
        incoming.path.clone(),
        incoming.content_type.clone(),
        &incoming.blob,
        now,
    );
    asset.component_id = incoming.component_id;
    asset.created_by = incoming.created_by;
    asset.last_downloaded_at = previous.and_then(|old| old.last_downloaded_at);
    asset.created_at = previous.map_or(now, |old| old.created_at);
    if incoming.insert_blob {
        state.blobs.insert(incoming.blob.id, incoming.blob.clone());
    }
    state
        .asset_index
        .insert((incoming.repository_id, incoming.path.clone()), asset.id);
    state.assets.insert(asset.id, asset.clone());
    // The blob is referenced again, and the one this write replaced may have lost its last
    // reference (the `assets_blob_ref` trigger).
    state.orphaned_at.remove(&asset.blob_id);
    if let Some(old) = previous.filter(|old| old.blob_id != asset.blob_id) {
        state.note_blob_unreferenced(old.blob_id);
    }
    if let Some(component) = incoming
        .component_id
        .and_then(|id| state.components.get_mut(&id))
    {
        component.updated_at = now;
    }
    // The replaced blob is handed back for GC only when this write took its last reference:
    // after `link` a blob can be shared, and a caller that deletes the row of a shared blob
    // would hit the foreign key.
    let replaced = previous
        .filter(|old| old.blob_id != asset.blob_id)
        .filter(|old| !state.assets.values().any(|a| a.blob_id == old.blob_id))
        .and_then(|old| state.blobs.get(&old.blob_id))
        .cloned();
    let mut events = options.events.clone();
    events.extend(near_threshold_events(
        state,
        repository,
        &scopes_before,
        options,
    ));
    state.push_events(&events, now);
    let created = previous.is_none();
    PublishOutcome::new(asset, replaced, created)
}

/// `quota.near_threshold`: fires once when a successful write takes a scope from below its
/// warn percentage to at or above it, written in the same transaction. The edge state is
/// cleared by the delete path (see `settle_after_delete`).
fn near_threshold_events(
    state: &mut State,
    repository: Option<&rustryx_core::model::Repository>,
    before: &[(QuotaScope, u64)],
    options: &WriteOptions,
) -> Vec<EventDraft> {
    let mut drafts = Vec::new();
    for (scope, used_before) in before {
        let Some(limit) = state.scope_limit(*scope).filter(|limit| *limit > 0) else {
            continue;
        };
        let (used, _) = state.scope_usage(*scope);
        let warn = state.warn_percent(*scope);
        let percent = percent_of(used, limit);
        if used > *used_before && percent >= u64::from(warn) && state.quota_warned.insert(*scope) {
            let actor = options
                .actor
                .clone()
                .unwrap_or_else(|| ActorRef::system("quota"));
            let mut draft = EventDraft::new(
                EventKind::QuotaNearThreshold,
                actor,
                serde_json::json!({
                    "scope": scope,
                    "limit_bytes": limit,
                    "used_bytes": used,
                    "percent": percent,
                }),
            );
            draft.repository_id = repository.map(|r| r.id);
            draft.project_id = match scope {
                QuotaScope::Project(id) => Some(*id),
                QuotaScope::Repository(_) => repository.and_then(|r| state.project_of(r)),
            };
            draft.request_id.clone_from(&options.request_id);
            drafts.push(draft);
        }
    }
    drafts
}

/// Whole percent of `limit` that `used` is, rounded down, without overflow.
pub(crate) fn percent_of(used: u64, limit: u64) -> u64 {
    let percent = u128::from(used) * 100 / u128::from(limit.max(1));
    u64::try_from(percent).unwrap_or(u64::MAX)
}

/// Clears the warned edge of the scopes that dropped below `warn_percent - 5` after a
/// delete, so the next climb can fire the event again.
pub(crate) fn settle_after_delete(state: &mut State, scopes: &[QuotaScope]) {
    for scope in scopes {
        let Some(limit) = state.scope_limit(*scope).filter(|limit| *limit > 0) else {
            state.quota_warned.remove(scope);
            continue;
        };
        let (used, _) = state.scope_usage(*scope);
        let floor = u64::from(state.warn_percent(*scope)).saturating_sub(5);
        if percent_of(used, limit) < floor {
            state.quota_warned.remove(scope);
        }
    }
}
