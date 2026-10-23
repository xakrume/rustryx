//! Builders for domain values used across the test modules.

use rustryx_core::identity::{Action, Privilege, PrivilegeGrant, Role};
use rustryx_core::ids::{AssetId, BlobId, BlobStoreId, PrivilegeId, RepositoryId, RoleId};
use rustryx_core::model::{
    BlobRecord, BlobStoreDef, BlobStoreKind, Checksums, NewAsset, NewComponent, RepoFormat,
    RepoType, Repository,
};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{BlobStoreRepo, RepositoryRepo};
use rustryx_db::PgStore;
use time::OffsetDateTime;
use uuid::Uuid;

/// Current time truncated to microseconds, the precision of `timestamptz`, so that a value
/// compares equal after a round trip.
pub fn now() -> OffsetDateTime {
    let t = OffsetDateTime::now_utc();
    t.replace_nanosecond(t.nanosecond() / 1000 * 1000).unwrap()
}

pub fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

pub fn blob_store(name: &str) -> BlobStoreDef {
    BlobStoreDef::new(
        BlobStoreId::generate(),
        name,
        BlobStoreKind::Local,
        serde_json::json!({ "path": "/var/lib/rustryx/blobs" }),
        now(),
    )
}

pub fn hosted_repository(name: &str, store: BlobStoreId) -> Repository {
    Repository::new(
        RepositoryId::generate(),
        RepoName::parse(name).unwrap(),
        RepoFormat::Maven2,
        RepoType::Hosted,
        store,
        now(),
    )
}

pub fn checksums(seed: Uuid) -> Checksums {
    let simple = seed.simple().to_string();
    Checksums {
        sha1: format!("{simple:0>40}"),
        sha256: format!("{simple:0>64}"),
        md5: simple,
    }
}

pub fn blob(store: BlobStoreId, size: u64) -> BlobRecord {
    let id = BlobId::generate();
    BlobRecord {
        id,
        blob_store_id: store,
        key: Uuid::now_v7().to_string(),
        size_bytes: size,
        checksums: checksums(id.as_uuid()),
        created_at: now(),
    }
}

pub fn new_asset(repo: RepositoryId, path: &str, blob: BlobRecord) -> NewAsset {
    NewAsset::new(repo, path, "application/octet-stream", blob)
}

pub fn new_component(
    repo: RepositoryId,
    group: Option<&str>,
    name: &str,
    version: Option<&str>,
) -> NewComponent {
    let mut new = NewComponent::new(repo, RepoFormat::Maven2, name);
    new.group_id = group.map(ToOwned::to_owned);
    new.version = version.map(ToOwned::to_owned);
    new
}

pub fn role(name: &str, privileges: Vec<PrivilegeId>, nested: Vec<RoleId>) -> Role {
    Role {
        id: RoleId::generate(),
        name: name.to_owned(),
        description: String::new(),
        builtin: false,
        privilege_ids: privileges,
        nested_role_ids: nested,
    }
}

pub fn view_privilege(name: &str, repository: &str, actions: Vec<Action>) -> Privilege {
    Privilege {
        id: PrivilegeId::generate(),
        name: name.to_owned(),
        description: String::new(),
        builtin: false,
        grant: PrivilegeGrant::RepositoryView {
            format: None,
            repository: repository.to_owned(),
        },
        actions,
    }
}

/// A blob store and a hosted repository, both persisted.
pub async fn seed_repository(store: &PgStore) -> (BlobStoreDef, Repository) {
    let blob_store = blob_store(&unique("store"));
    store.blob_stores.create(&blob_store).await.unwrap();
    let repository = hosted_repository(&unique("repo"), blob_store.id);
    store.repositories.create(&repository).await.unwrap();
    (blob_store, repository)
}

/// Publishes one asset and returns its id.
pub async fn publish(
    store: &PgStore,
    repo: RepositoryId,
    store_id: BlobStoreId,
    path: &str,
) -> AssetId {
    use rustryx_core::ports::AssetRepo;
    store
        .assets
        .publish(&new_asset(repo, path, blob(store_id, 10)))
        .await
        .unwrap()
        .asset
        .id
}

/// An external (LDAP) account without a password hash: the cheapest valid user, for tests that
/// need a `created_by` or `updated_by` to point at.
pub async fn user(store: &PgStore) -> rustryx_core::ids::UserId {
    use rustryx_core::identity::{NewUser, UserSource};
    use rustryx_core::ports::UserRepo;
    store
        .users
        .create(&NewUser {
            username: unique("user"),
            source: UserSource::Ldap,
            issuer: None,
            subject: None,
            email: None,
            display_name: None,
            password_hash: None,
        })
        .await
        .unwrap()
        .id
}
