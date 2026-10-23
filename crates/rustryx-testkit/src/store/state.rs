//! Tables behind [`super::MemStore`].

use std::collections::{BTreeMap, HashMap};

use rustryx_core::identity::{
    ApiToken, ContentSelector, IdpGroupMapping, Privilege, Role, RoleOrigin, User,
};
use rustryx_core::ids::{
    ApiTokenId, AssetId, BlobId, BlobStoreId, ComponentId, ContentSelectorId, IdpGroupMappingId,
    PrivilegeId, RepositoryId, RoleId, UserId,
};
use rustryx_core::model::{Asset, BlobRecord, BlobStoreDef, Component, Repository};
use rustryx_core::ports::{LoginKey, LoginState};

#[derive(Default)]
pub(crate) struct State {
    pub(crate) repositories: HashMap<RepositoryId, Repository>,
    pub(crate) blob_stores: HashMap<BlobStoreId, BlobStoreDef>,
    pub(crate) blobs: HashMap<BlobId, BlobRecord>,
    pub(crate) components: HashMap<ComponentId, Component>,
    pub(crate) assets: HashMap<AssetId, Asset>,
    /// Ordered `(repository, path)` index: gives exact lookup and prefix scans, and
    /// keeps path uniqueness a property of the structure rather than of every writer.
    pub(crate) asset_index: BTreeMap<(RepositoryId, String), AssetId>,
    pub(crate) users: HashMap<UserId, User>,
    /// Role assignments per user. One role may appear under several origins.
    pub(crate) user_roles: HashMap<UserId, Vec<(RoleId, RoleOrigin)>>,
    pub(crate) roles: HashMap<RoleId, Role>,
    pub(crate) privileges: HashMap<PrivilegeId, Privilege>,
    pub(crate) selectors: HashMap<ContentSelectorId, ContentSelector>,
    pub(crate) tokens: HashMap<ApiTokenId, ApiToken>,
    pub(crate) mappings: HashMap<IdpGroupMappingId, IdpGroupMapping>,
    pub(crate) login_attempts: HashMap<LoginKey, LoginState>,
}

impl State {
    /// Removes an asset row and its index entry together, so the two cannot drift.
    pub(crate) fn remove_asset(&mut self, id: AssetId) -> Option<Asset> {
        let asset = self.assets.remove(&id)?;
        self.asset_index
            .remove(&(asset.repository_id, asset.path.clone()));
        Some(asset)
    }
}
