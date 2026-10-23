//! Shared fixtures of the authorization tests.
//!
//! The metadata store is the testkit's `MemStore` (seeded with the built-in
//! roles) wrapped in [`SpyStore`], which counts reads and can fail them
//! selectively. The wrapper exists because the tests must prove that the caches
//! save store calls and that a failing store never turns into an allow.

// Each test binary uses a different subset of these helpers, and a helper that is
// unused in one binary would otherwise warn there.
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::identity::{
    Action, ContentSelector, Principal, Privilege, PrivilegeGrant, Role, RoleRef,
};
use rustryx_core::ids::{ContentSelectorId, PrivilegeId, RoleId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::{Coordinate, ResourceRef};
use rustryx_core::ports::{RoleRepo, SelectorRepo};
use rustryx_rbac::{Rbac, RbacConfig};
use rustryx_testkit::fixtures::principal_with_roles;
use rustryx_testkit::{MemInvalidationBus, MemStore};
use time::OffsetDateTime;

/// Store wrapper that counts the reads the authorizer makes.
#[derive(Clone)]
pub struct SpyStore {
    pub inner: MemStore,
    pub privilege_reads: Arc<AtomicUsize>,
    pub role_name_reads: Arc<AtomicUsize>,
    pub selector_reads: Arc<AtomicUsize>,
    pub fail_selectors: Arc<AtomicBool>,
}

impl SpyStore {
    pub fn new(inner: MemStore) -> Self {
        Self {
            inner,
            privilege_reads: Arc::default(),
            role_name_reads: Arc::default(),
            selector_reads: Arc::default(),
            fail_selectors: Arc::default(),
        }
    }

    pub fn privilege_reads(&self) -> usize {
        self.privilege_reads.load(Ordering::SeqCst)
    }

    pub fn role_name_reads(&self) -> usize {
        self.role_name_reads.load(Ordering::SeqCst)
    }

    pub fn selector_reads(&self) -> usize {
        self.selector_reads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RoleRepo for SpyStore {
    async fn get(&self, id: RoleId) -> Result<Option<Role>, StoreError> {
        RoleRepo::get(&self.inner, id).await
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Role>, StoreError> {
        self.role_name_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get_by_name(name).await
    }

    async fn list(&self) -> Result<Vec<Role>, StoreError> {
        RoleRepo::list(&self.inner).await
    }

    async fn create(&self, role: &Role) -> Result<(), StoreError> {
        RoleRepo::create(&self.inner, role).await
    }

    async fn update(&self, role: &Role) -> Result<(), StoreError> {
        RoleRepo::update(&self.inner, role).await
    }

    async fn delete(&self, id: RoleId) -> Result<(), StoreError> {
        RoleRepo::delete(&self.inner, id).await
    }

    async fn effective_privileges(&self, roles: &[RoleId]) -> Result<Vec<Privilege>, StoreError> {
        self.privilege_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.effective_privileges(roles).await
    }
}

#[async_trait]
impl SelectorRepo for SpyStore {
    async fn get(&self, id: ContentSelectorId) -> Result<Option<ContentSelector>, StoreError> {
        self.selector_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_selectors.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("selector table unavailable".into()));
        }
        SelectorRepo::get(&self.inner, id).await
    }

    async fn list(&self) -> Result<Vec<ContentSelector>, StoreError> {
        SelectorRepo::list(&self.inner).await
    }

    async fn create(&self, selector: &ContentSelector) -> Result<(), StoreError> {
        SelectorRepo::create(&self.inner, selector).await
    }

    async fn update(&self, selector: &ContentSelector) -> Result<(), StoreError> {
        SelectorRepo::update(&self.inner, selector).await
    }

    async fn delete(&self, id: ContentSelectorId) -> Result<(), StoreError> {
        SelectorRepo::delete(&self.inner, id).await
    }
}

/// An authorizer over a seeded store.
pub struct Env {
    pub store: SpyStore,
    pub bus: MemInvalidationBus,
    pub rbac: Rbac,
}

impl Env {
    /// Store seeded with `nx-admin`, `nx-developer` and `nx-anonymous`. The store
    /// publishes its RBAC changes on `bus`, but nothing listens unless the test
    /// starts a listener.
    pub fn new() -> Self {
        Self::with_config(&RbacConfig::default())
    }

    pub fn with_config(config: &RbacConfig) -> Self {
        let bus = MemInvalidationBus::new();
        let mem = MemStore::with_builtin_roles().with_invalidation_bus(bus.clone());
        let store = SpyStore::new(mem);
        let rbac = Rbac::new(Arc::new(store.clone()), Arc::new(store.clone()), config);
        Self { store, bus, rbac }
    }

    /// Adds a privilege and returns its id.
    pub async fn privilege(
        &self,
        name: &str,
        grant: PrivilegeGrant,
        actions: &[Action],
    ) -> PrivilegeId {
        let privilege = Privilege {
            id: PrivilegeId::generate(),
            name: name.to_owned(),
            description: String::new(),
            builtin: false,
            grant,
            actions: actions.to_vec(),
        };
        let id = privilege.id;
        rustryx_core::ports::PrivilegeRepo::create(&self.store.inner, &privilege)
            .await
            .unwrap();
        id
    }

    /// Adds a role holding `privileges` and nesting `nested`.
    pub async fn role(&self, name: &str, privileges: &[PrivilegeId], nested: &[RoleId]) -> RoleId {
        let role = Role {
            id: RoleId::generate(),
            name: name.to_owned(),
            description: String::new(),
            builtin: false,
            privilege_ids: privileges.to_vec(),
            nested_role_ids: nested.to_vec(),
        };
        let id = role.id;
        RoleRepo::create(&self.store.inner, &role).await.unwrap();
        id
    }

    /// Adds a selector and returns its id.
    pub async fn selector(&self, name: &str, expression: &str) -> ContentSelectorId {
        let selector = ContentSelector {
            id: ContentSelectorId::generate(),
            name: name.to_owned(),
            description: String::new(),
            expression: expression.to_owned(),
            updated_at: OffsetDateTime::now_utc(),
        };
        let id = selector.id;
        SelectorRepo::create(&self.store.inner, &selector)
            .await
            .unwrap();
        id
    }

    /// A role with one privilege, in one call.
    pub async fn simple_role(
        &self,
        name: &str,
        grant: PrivilegeGrant,
        actions: &[Action],
    ) -> RoleId {
        let privilege = self
            .privilege(&format!("{name}-privilege"), grant, actions)
            .await;
        self.role(name, &[privilege], &[]).await
    }
}

/// An authenticated principal holding exactly the given roles (by id and name).
pub fn principal(roles: &[(RoleId, &str)]) -> Principal {
    let mut principal = principal_with_roles(&[]);
    principal.roles = roles
        .iter()
        .map(|(id, name)| RoleRef {
            id: *id,
            name: (*name).to_owned(),
        })
        .collect();
    principal
}

/// Principal holding only the built-in role of that name, with the seeded id.
pub fn builtin_principal(name: &str) -> Principal {
    principal_with_roles(&[name])
}

pub fn view(repository: &str, format: Option<RepoFormat>) -> PrivilegeGrant {
    PrivilegeGrant::RepositoryView {
        format,
        repository: repository.to_owned(),
    }
}

pub fn selector_grant(repository: &str, selector_id: ContentSelectorId) -> PrivilegeGrant {
    PrivilegeGrant::RepositoryContentSelector {
        repository: repository.to_owned(),
        selector_id,
    }
}

pub fn resource(repository: &str, format: RepoFormat, path: &str) -> ResourceRef {
    ResourceRef {
        repository: repository.to_owned(),
        format,
        path: path.to_owned(),
        coordinate: None,
    }
}

pub fn resource_with_coordinate(
    repository: &str,
    format: RepoFormat,
    path: &str,
    group: Option<&str>,
    name: Option<&str>,
    version: Option<&str>,
) -> ResourceRef {
    ResourceRef {
        coordinate: Some(Coordinate {
            group: group.map(str::to_owned),
            name: name.map(str::to_owned),
            version: version.map(str::to_owned),
        }),
        ..resource(repository, format, path)
    }
}
