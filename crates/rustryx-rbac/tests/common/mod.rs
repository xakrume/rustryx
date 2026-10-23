//! Shared fixtures of the authorization tests.
//!
//! The metadata store is the testkit's `MemStore` (seeded with the built-in
//! roles) wrapped in [`SpyStore`], which counts reads and can fail them
//! selectively. The wrapper exists because the tests must prove that the caches
//! save store calls and that a failing store never turns into an allow.

// Each test binary uses a different subset of these helpers, and a helper that is
// unused in one binary would otherwise warn there.
#![allow(dead_code)]

pub mod recorder;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::identity::{
    Action, AuthMethod, ContentSelector, Principal, Privilege, PrivilegeGrant, Role, RoleRef,
    UserSource,
};
use rustryx_core::ids::{ContentSelectorId, PrivilegeId, RoleId};
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::auth::{Coordinate, ResourceRef};
use rustryx_core::ports::{RoleRepo, SelectorRepo};
use rustryx_rbac::{Rbac, RbacConfig};
use rustryx_testkit::fixtures::{principal_with_roles, repository};
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

/// The configuration most tests want: the global anonymous switch ON, so that the repository
/// flag alone decides (`open_resource` against `resource`). The tests of the switch itself
/// build their own config.
pub fn anonymous_config() -> RbacConfig {
    RbacConfig {
        anonymous_enabled: true,
        ..RbacConfig::default()
    }
}

impl Env {
    /// Store seeded with `nx-admin`, `nx-developer` and `nx-anonymous`, anonymous switch on.
    /// The store publishes its RBAC changes on `bus`, but nothing listens unless the test
    /// starts a listener.
    pub fn new() -> Self {
        Self::with_config(&anonymous_config())
    }

    /// Like [`Env::new`] with an explicit configuration (the switch is whatever it says).
    pub fn with_config(config: &RbacConfig) -> Self {
        let bus = MemInvalidationBus::new();
        let mem = MemStore::with_builtin_roles().with_invalidation_bus(bus.clone());
        let store = SpyStore::new(mem);
        let rbac = Rbac::new(Arc::new(store.clone()), Arc::new(store.clone()), config);
        Self { store, bus, rbac }
    }

    /// A store that does not seed the built-in roles, so a test can define `nx-anonymous` (or
    /// any built-in name) with exactly the privileges it wants to prove are not enough.
    /// Anonymous switch on.
    pub fn bare() -> Self {
        Self::bare_with_config(&anonymous_config())
    }

    pub fn bare_with_config(config: &RbacConfig) -> Self {
        let bus = MemInvalidationBus::new();
        let store = SpyStore::new(MemStore::new().with_invalidation_bus(bus.clone()));
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

/// A robot or scan token principal that carries the given role references. A real robot has
/// none; the tests hand it some to prove they are not evidence of anything.
pub fn robot(method: AuthMethod, roles: &[RoleRef], scopes: &[&str]) -> Principal {
    let mut principal = Principal::new("robot+ci", UserSource::Local, method);
    principal.roles = roles.to_vec();
    principal.token_scopes = Some(scopes.iter().map(|s| (*s).to_owned()).collect());
    principal
}

/// An external principal (OIDC or LDAP) holding the given roles and external groups.
pub fn external(source: UserSource, roles: &[RoleRef], groups: &[&str]) -> Principal {
    let method = match source {
        UserSource::Ldap => AuthMethod::BasicLdap,
        _ => AuthMethod::OidcBearer,
    };
    let mut principal = Principal::new("external-user", source, method);
    principal.user_id = Some(rustryx_core::ids::UserId::generate());
    principal.roles = roles.to_vec();
    principal.external_groups = groups.iter().map(|g| (*g).to_owned()).collect();
    principal
}

/// The principal with an API token carrying exactly these scopes.
pub fn with_scopes(mut principal: Principal, scopes: &[&str]) -> Principal {
    principal.token_scopes = Some(scopes.iter().map(|s| (*s).to_owned()).collect());
    principal
}

/// The role reference of a built-in role (the seeded id and the real name).
pub fn builtin_ref(name: &str) -> RoleRef {
    builtin_principal(name).roles[0].clone()
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

/// A resource in a repository that is CLOSED to anonymous callers (the default of every
/// repository).
pub fn resource(repository: &str, format: RepoFormat, path: &str) -> ResourceRef {
    rustryx_testkit::fixtures::resource(repository, format, path)
}

/// A resource in a repository that opted in to anonymous access (`allow_anonymous`).
pub fn open_resource(repository: &str, format: RepoFormat, path: &str) -> ResourceRef {
    let mut resource = resource(repository, format, path);
    resource.allow_anonymous = true;
    resource
}

/// A fixture repository, closed to anonymous callers.
pub fn repo(name: &str, format: RepoFormat) -> Repository {
    repository(name, format, RepoType::Hosted)
}

/// A fixture repository that opted in to anonymous access.
pub fn open_repo(name: &str, format: RepoFormat) -> Repository {
    let mut repository = repo(name, format);
    repository.allow_anonymous = true;
    repository
}

pub fn coordinate(group: Option<&str>, name: Option<&str>, version: Option<&str>) -> Coordinate {
    Coordinate::new(
        group.map(str::to_owned),
        name.map(str::to_owned),
        version.map(str::to_owned),
    )
}

pub fn resource_with_coordinate(
    repository: &str,
    format: RepoFormat,
    path: &str,
    group: Option<&str>,
    name: Option<&str>,
    version: Option<&str>,
) -> ResourceRef {
    resource(repository, format, path).with_coordinate(coordinate(group, name, version))
}

/// Role store whose first `effective_privileges` call reads the data, then parks until
/// released: a load that is still running when a change (or a second caller) arrives.
pub struct SlowFirstLoad {
    pub inner: SpyStore,
    pub park_next: AtomicBool,
    pub parked: Arc<tokio::sync::Notify>,
    pub release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl RoleRepo for SlowFirstLoad {
    async fn get(&self, id: RoleId) -> Result<Option<Role>, StoreError> {
        RoleRepo::get(&self.inner, id).await
    }
    async fn get_by_name(&self, name: &str) -> Result<Option<Role>, StoreError> {
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
        let snapshot = self.inner.effective_privileges(roles).await;
        if self.park_next.swap(false, Ordering::SeqCst) {
            self.parked.notify_one();
            self.release.notified().await;
        }
        snapshot
    }
}

impl SlowFirstLoad {
    /// A double over `inner` whose next privilege load parks.
    pub fn parking(inner: SpyStore) -> Self {
        Self {
            inner,
            park_next: AtomicBool::new(true),
            parked: Arc::default(),
            release: Arc::default(),
        }
    }
}
