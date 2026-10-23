use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use rustryx_core::ids::RepositoryId;
use rustryx_core::model::{RepoFormat, RepoType};
use rustryx_core::names::RepoName;
use rustryx_core::ports::BlobKey;
use serde_json::json;
use time::OffsetDateTime;

use super::*;
use crate::secrets::MapSecretResolver;

struct FakeRepo {
    defs: Mutex<HashMap<BlobStoreId, BlobStoreDef>>,
    gets: AtomicUsize,
}

impl FakeRepo {
    fn with(defs: impl IntoIterator<Item = BlobStoreDef>) -> Arc<Self> {
        let map = defs.into_iter().map(|d| (d.id, d)).collect();
        Arc::new(Self {
            defs: Mutex::new(map),
            gets: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl BlobStoreRepo for FakeRepo {
    async fn create(&self, store: &BlobStoreDef) -> Result<(), StoreError> {
        self.defs.lock().unwrap().insert(store.id, store.clone());
        Ok(())
    }

    async fn get(&self, id: BlobStoreId) -> Result<Option<BlobStoreDef>, StoreError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        // Widen the race window so concurrent first calls overlap.
        tokio::task::yield_now().await;
        Ok(self.defs.lock().unwrap().get(&id).cloned())
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<BlobStoreDef>, StoreError> {
        Ok(self
            .defs
            .lock()
            .unwrap()
            .values()
            .find(|d| d.name == name)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<BlobStoreDef>, StoreError> {
        Ok(self.defs.lock().unwrap().values().cloned().collect())
    }

    async fn update(&self, store: &BlobStoreDef) -> Result<(), StoreError> {
        self.defs.lock().unwrap().insert(store.id, store.clone());
        Ok(())
    }

    async fn delete(&self, id: BlobStoreId) -> Result<(), StoreError> {
        self.defs.lock().unwrap().remove(&id);
        Ok(())
    }
}

fn def(kind: BlobStoreKind, config: serde_json::Value) -> BlobStoreDef {
    BlobStoreDef::new(
        BlobStoreId::generate(),
        "test",
        kind,
        config,
        OffsetDateTime::UNIX_EPOCH,
    )
}

/// The floor is off: these tests write on whatever disk the developer has, which may well be
/// below 10% free. The floor itself is tested with a fake probe further down.
fn local_def(dir: &tempfile::TempDir) -> BlobStoreDef {
    def(
        BlobStoreKind::Local,
        json!({"path": dir.path(), "min_free_percent": 0}),
    )
}

fn s3_def(config: serde_json::Value) -> BlobStoreDef {
    def(BlobStoreKind::S3, config)
}

fn registry(repo: Arc<FakeRepo>) -> BlobStoreRegistry {
    let secrets = MapSecretResolver::new([("AK", "access"), ("SK", "secret"), ("TK", "token")]);
    BlobStoreRegistry::new(repo, Arc::new(secrets))
}

#[tokio::test]
async fn builds_a_local_store_once_and_caches_it() {
    let dir = tempfile::tempdir().unwrap();
    let definition = local_def(&dir);
    let repo = FakeRepo::with([definition.clone()]);
    let registry = registry(Arc::clone(&repo));

    let first = registry.store_by_id(definition.id).await.unwrap();
    let second = registry.store_by_id(definition.id).await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.kind(), BlobStoreKind::Local);
    assert_eq!(repo.gets.load(Ordering::SeqCst), 1);
    assert_eq!(registry.cached(), 1);
    first.health().await.unwrap();
}

#[tokio::test]
async fn concurrent_first_calls_build_one_instance() {
    let dir = tempfile::tempdir().unwrap();
    let definition = local_def(&dir);
    let repo = FakeRepo::with([definition.clone()]);
    let registry = Arc::new(registry(Arc::clone(&repo)));

    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let registry = Arc::clone(&registry);
            let id = definition.id;
            tokio::spawn(async move { registry.store_by_id(id).await.unwrap() })
        })
        .collect();
    let mut stores = Vec::new();
    for task in tasks {
        stores.push(task.await.unwrap());
    }
    assert!(
        stores
            .windows(2)
            .all(|pair| Arc::ptr_eq(&pair[0], &pair[1]))
    );
    assert_eq!(
        repo.gets.load(Ordering::SeqCst),
        1,
        "definition loaded once"
    );
}

#[tokio::test]
async fn must_not_cache_unknown_ids() {
    let registry = registry(FakeRepo::with([]));
    let id = BlobStoreId::generate();
    let err = registry.store_by_id(id).await.err().unwrap();
    assert!(matches!(err, RegistryError::Unknown(unknown) if unknown == id));
    assert_eq!(
        registry.cached(),
        0,
        "a failed lookup must not leave a slot behind"
    );
    assert!(matches!(
        DomainError::from(err),
        DomainError::NotFound {
            entity: "blob store"
        }
    ));
}

#[tokio::test]
async fn failed_build_is_retried_after_the_cause_is_fixed() {
    let dir = tempfile::tempdir().unwrap();
    let definition = local_def(&dir);
    let mut broken = definition.clone();
    broken.config = json!({"path": "relative/dir"});
    let repo = FakeRepo::with([broken]);
    let registry = registry(Arc::clone(&repo));

    assert!(matches!(
        registry.store_by_id(definition.id).await.err().unwrap(),
        RegistryError::Build(BuildError::Invalid(_))
    ));
    assert_eq!(registry.cached(), 0);

    repo.create(&definition).await.unwrap();
    assert!(registry.store_by_id(definition.id).await.is_ok());
}

#[tokio::test]
async fn s3_credentials_are_resolved_from_references_not_from_json() {
    let definition = s3_def(json!({
        "bucket": "artifacts",
        "endpoint": "https://s3.example.test",
        "access_key_id_ref": "AK",
        "secret_access_key_ref": "SK",
        "session_token_ref": "TK",
    }));
    let registry = registry(FakeRepo::with([definition.clone()]));
    let store = registry.store_by_id(definition.id).await.unwrap();
    assert_eq!(store.kind(), BlobStoreKind::S3);
}

#[tokio::test]
async fn s3_without_credential_references_uses_the_ambient_chain() {
    let definition = s3_def(json!({"bucket": "artifacts"}));
    let registry = registry(FakeRepo::with([definition.clone()]));
    assert!(registry.store_by_id(definition.id).await.is_ok());
}

#[tokio::test]
async fn missing_secret_is_a_build_error_naming_only_the_reference() {
    let definition = s3_def(json!({
        "bucket": "artifacts",
        "access_key_id_ref": "AK",
        "secret_access_key_ref": "NOT_THERE",
    }));
    let registry = registry(FakeRepo::with([definition.clone()]));
    let err = registry.store_by_id(definition.id).await.err().unwrap();
    assert!(matches!(err, RegistryError::Build(BuildError::Secret(_))));
    assert!(err.to_string().contains("NOT_THERE"));
    assert_eq!(registry.cached(), 0);
}

#[tokio::test]
async fn must_not_expose_secret_values_when_the_definition_is_invalid() {
    let definition = s3_def(json!({"bucket": "b", "secret_access_key": "AKIA-leaked-value"}));
    let registry = registry(FakeRepo::with([definition.clone()]));
    let err = registry.store_by_id(definition.id).await.err().unwrap();
    assert!(!err.to_string().contains("AKIA-leaked-value"));
    assert!(!format!("{err:?}").contains("AKIA-leaked-value"));
    let domain = DomainError::from(err);
    assert!(!domain.to_string().contains("AKIA-leaked-value"));
}

#[tokio::test]
async fn invalidate_forces_a_rebuild_and_old_users_keep_working() {
    let dir = tempfile::tempdir().unwrap();
    let definition = local_def(&dir);
    let repo = FakeRepo::with([definition.clone()]);
    let registry = registry(Arc::clone(&repo));

    let old = registry.store_by_id(definition.id).await.unwrap();
    registry.invalidate(definition.id);
    assert_eq!(registry.cached(), 0);
    let new = registry.store_by_id(definition.id).await.unwrap();
    assert!(!Arc::ptr_eq(&old, &new));
    assert_eq!(repo.gets.load(Ordering::SeqCst), 2);
    assert!(
        old.head(&BlobKey::new("still-usable"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn register_replaces_the_cached_store_without_touching_the_repo() {
    let dir = tempfile::tempdir().unwrap();
    let definition = local_def(&dir);
    let repo = FakeRepo::with([]);
    let registry = registry(Arc::clone(&repo));

    let registered = registry.register(&definition).await.unwrap();
    let fetched = registry.store_by_id(definition.id).await.unwrap();
    assert!(Arc::ptr_eq(&registered, &fetched));
    assert_eq!(repo.gets.load(Ordering::SeqCst), 0);

    let again = registry.register(&definition).await.unwrap();
    assert!(!Arc::ptr_eq(&registered, &again));
}

#[tokio::test]
async fn register_surfaces_build_errors() {
    let registry = registry(FakeRepo::with([]));
    let bad = s3_def(json!({"bucket": ""}));
    assert!(registry.register(&bad).await.is_err());
    assert_eq!(registry.cached(), 0);
}

#[test]
fn repo_and_build_errors_map_to_domain_errors() {
    let repo = RegistryError::Repo(StoreError::Unavailable("db down".into()));
    assert!(matches!(
        DomainError::from(repo),
        DomainError::Store(StoreError::Unavailable(_))
    ));
    let build = RegistryError::Build(BuildError::Invalid("local: bad".into()));
    assert!(matches!(DomainError::from(build), DomainError::Internal(_)));
}

#[tokio::test]
async fn shutdown_waits_for_cleanup_of_dropped_writers() {
    let dir = tempfile::tempdir().unwrap();
    let definition = local_def(&dir);
    let registry = registry(FakeRepo::with([definition.clone()]));
    let store = registry.store_by_id(definition.id).await.unwrap();

    let mut writer = store.begin(&BlobKey::new("abandoned"), None).await.unwrap();
    writer
        .write(bytes::Bytes::from_static(b"partial"))
        .await
        .unwrap();
    drop(writer);
    registry.shutdown().await;

    let staging = dir.path().join("_staging");
    let mut entries = tokio::fs::read_dir(&staging).await.unwrap();
    assert!(
        entries.next_entry().await.unwrap().is_none(),
        "staging must be empty after shutdown"
    );
}

#[tokio::test]
async fn shutdown_of_an_empty_registry_returns() {
    registry(FakeRepo::with([])).shutdown().await;
}

fn repository_on(store: BlobStoreId) -> Repository {
    Repository::new(
        RepositoryId::generate(),
        RepoName::parse("releases").unwrap(),
        RepoFormat::Raw,
        RepoType::Hosted,
        store,
        OffsetDateTime::UNIX_EPOCH,
    )
}

#[tokio::test]
async fn a_repository_is_served_by_the_store_its_definition_names() {
    let dir = tempfile::tempdir().unwrap();
    let definition = local_def(&dir);
    let registry = registry(FakeRepo::with([definition.clone()]));

    let by_repository = registry
        .store_for_repository(&repository_on(definition.id))
        .await
        .unwrap();
    let by_id = registry.store_by_id(definition.id).await.unwrap();
    assert!(Arc::ptr_eq(&by_repository, &by_id), "one instance per id");

    let orphan = repository_on(BlobStoreId::generate());
    assert!(matches!(
        registry.store_for_repository(&orphan).await.err(),
        Some(RegistryError::Unknown(_))
    ));
}

#[tokio::test]
async fn the_definition_is_read_once_and_kept_with_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut definition = local_def(&dir);
    definition.quota_bytes = Some(4096);
    let repo = FakeRepo::with([definition.clone()]);
    let registry = registry(Arc::clone(&repo));

    for _ in 0..5 {
        let cached = registry.definition(definition.id).await.unwrap();
        assert_eq!(cached.quota_bytes, Some(4096));
        assert_eq!(cached.name, "test");
    }
    registry.store_by_id(definition.id).await.unwrap();
    assert_eq!(
        repo.gets.load(Ordering::SeqCst),
        1,
        "the write path may ask for the quota on every upload without a query"
    );

    // An edit reaches the node as an invalidation; the next read sees the new quota.
    let mut edited = definition.clone();
    edited.quota_bytes = Some(8192);
    repo.update(&edited).await.unwrap();
    assert_eq!(
        registry
            .definition(definition.id)
            .await
            .unwrap()
            .quota_bytes,
        Some(4096),
        "the cache holds until it is told"
    );
    registry.invalidate(definition.id);
    assert_eq!(
        registry
            .definition(definition.id)
            .await
            .unwrap()
            .quota_bytes,
        Some(8192)
    );
}

#[tokio::test]
async fn a_reset_drops_every_cached_store() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let (a, b) = (local_def(&dir_a), local_def(&dir_b));
    let registry = registry(FakeRepo::with([a.clone(), b.clone()]));
    registry.store_by_id(a.id).await.unwrap();
    registry.store_by_id(b.id).await.unwrap();
    assert_eq!(registry.cached(), 2);
    registry.invalidate_all();
    assert_eq!(registry.cached(), 0);
}

#[tokio::test]
async fn kinds_this_build_cannot_serve_are_refused_by_name() {
    for (kind, label) in [
        (BlobStoreKind::Azure, "azure"),
        (BlobStoreKind::Group, "group"),
    ] {
        let definition = def(kind, json!({}));
        let registry = registry(FakeRepo::with([definition.clone()]));
        let err = registry.store_by_id(definition.id).await.err().unwrap();
        match &err {
            RegistryError::Build(BuildError::UnsupportedKind(found)) => {
                assert_eq!(*found, label);
            }
            other => panic!("{other:?}"),
        }
        assert!(err.to_string().contains(label));
        assert_eq!(registry.cached(), 0);
    }
}

#[test]
fn registry_errors_map_onto_the_port_contract() {
    use crate::secrets::SecretError;
    let unavailable = |err: RegistryError| match BlobError::from(err) {
        BlobError::Unavailable(text) => text,
        other => panic!("expected Unavailable, got {other:?}"),
    };
    let backend = |err: RegistryError| matches!(BlobError::from(err), BlobError::Backend { .. });

    assert!(unavailable(RegistryError::Unknown(BlobStoreId::generate())).contains("not defined"));
    assert!(
        unavailable(RegistryError::Repo(StoreError::Unavailable("db".into())))
            .contains("could not be loaded")
    );
    assert!(
        unavailable(RegistryError::Build(BuildError::Secret(
            SecretError::Unavailable {
                reference: "R".into(),
                reason: "io".into()
            }
        )))
        .contains("secrets")
    );
    // A root that cannot be opened keeps its own, more specific error.
    assert!(matches!(
        BlobError::from(RegistryError::Build(BuildError::Open(
            BlobError::InsufficientStorage
        ))),
        BlobError::InsufficientStorage
    ));
    // Definitions that are wrong stay wrong: 500 class, detail kept for the logs.
    assert!(backend(RegistryError::Build(BuildError::Invalid(
        "x".into()
    ))));
    assert!(backend(RegistryError::Build(BuildError::UnsupportedKind(
        "group"
    ))));
    assert!(backend(RegistryError::Build(BuildError::Secret(
        SecretError::NotFound("R".into())
    ))));
}

#[tokio::test]
async fn a_local_store_built_by_the_registry_enforces_the_configured_floor() {
    use crate::disk::testing::FakeDisk;
    let dir = tempfile::tempdir().unwrap();
    // No `min_free_percent` in the JSON: the documented default of 10 percent applies.
    let definition = def(BlobStoreKind::Local, json!({"path": dir.path()}));
    let disk = Arc::new(FakeDisk::new(1000, 80));
    let registry = registry(FakeRepo::with([definition.clone()]))
        .with_disk_probe(Arc::clone(&disk) as Arc<dyn crate::disk::DiskProbe>);

    let store = registry.store_by_id(definition.id).await.unwrap();
    assert!(matches!(
        store.begin(&BlobKey::new("k"), None).await.err(),
        Some(BlobError::InsufficientStorage)
    ));
    // Reads and the health probe are untouched.
    store.health().await.unwrap();

    // A definition may lower the floor, and zero switches it off.
    let relaxed = def(
        BlobStoreKind::Local,
        json!({"path": dir.path(), "min_free_percent": 5}),
    );
    let store = registry.register(&relaxed).await.unwrap();
    store
        .begin(&BlobKey::new("k"), None)
        .await
        .unwrap()
        .abort()
        .await
        .unwrap();
}

#[tokio::test]
async fn a_floor_above_ninety_percent_is_a_build_error() {
    let dir = tempfile::tempdir().unwrap();
    let definition = def(
        BlobStoreKind::Local,
        json!({"path": dir.path(), "min_free_percent": 95}),
    );
    let registry = registry(FakeRepo::with([definition.clone()]));
    assert!(matches!(
        registry.store_by_id(definition.id).await.err(),
        Some(RegistryError::Build(BuildError::Invalid(_)))
    ));
}

#[tokio::test]
async fn shutdown_ends_the_disk_monitors_of_the_cached_stores() {
    use crate::disk::testing::FakeDisk;
    let dir = tempfile::tempdir().unwrap();
    let definition = local_def(&dir);
    let disk = Arc::new(FakeDisk::new(1000, 900));
    let registry = registry(FakeRepo::with([definition.clone()]))
        .with_disk_probe(Arc::clone(&disk) as Arc<dyn crate::disk::DiskProbe>);
    registry.store_by_id(definition.id).await.unwrap();
    let calls = disk.calls();
    assert!(calls >= 1, "the store sampled the disk while opening");
    registry.shutdown().await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while Arc::strong_count(&disk) > 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "the monitor thread still holds the probe"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn s3_presign_settings_reach_the_store() {
    let definition = s3_def(json!({
        "bucket": "artifacts",
        "endpoint": "https://s3.example.test",
        "presign": {"enabled": true, "ttl_secs": 120},
    }));
    let registry = registry(FakeRepo::with([definition.clone()]));
    let store = registry.store_by_id(definition.id).await.unwrap();
    let caps = store.capabilities();
    assert!(caps.can_presign && caps.lists_keys && !caps.lists_staging);

    let plain = s3_def(json!({"bucket": "artifacts"}));
    assert!(
        !registry
            .register(&plain)
            .await
            .unwrap()
            .capabilities()
            .can_presign
    );
}
