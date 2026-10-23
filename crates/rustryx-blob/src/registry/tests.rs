use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
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

    async fn delete(&self, id: BlobStoreId) -> Result<(), StoreError> {
        self.defs.lock().unwrap().remove(&id);
        Ok(())
    }
}

fn def(kind: BlobStoreKind, config: serde_json::Value) -> BlobStoreDef {
    BlobStoreDef {
        id: BlobStoreId::generate(),
        name: "test".into(),
        kind,
        config,
        quota_bytes: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn local_def(dir: &tempfile::TempDir) -> BlobStoreDef {
    def(BlobStoreKind::Local, json!({"path": dir.path()}))
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

    let first = registry.store_for(definition.id).await.unwrap();
    let second = registry.store_for(definition.id).await.unwrap();
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
            tokio::spawn(async move { registry.store_for(id).await.unwrap() })
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
    let err = registry.store_for(id).await.err().unwrap();
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
    let broken = BlobStoreDef {
        config: json!({"path": "relative/dir"}),
        ..definition.clone()
    };
    let repo = FakeRepo::with([broken]);
    let registry = registry(Arc::clone(&repo));

    assert!(matches!(
        registry.store_for(definition.id).await.err().unwrap(),
        RegistryError::Build(BuildError::Invalid(_))
    ));
    assert_eq!(registry.cached(), 0);

    repo.create(&definition).await.unwrap();
    assert!(registry.store_for(definition.id).await.is_ok());
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
    let store = registry.store_for(definition.id).await.unwrap();
    assert_eq!(store.kind(), BlobStoreKind::S3);
}

#[tokio::test]
async fn s3_without_credential_references_uses_the_ambient_chain() {
    let definition = s3_def(json!({"bucket": "artifacts"}));
    let registry = registry(FakeRepo::with([definition.clone()]));
    assert!(registry.store_for(definition.id).await.is_ok());
}

#[tokio::test]
async fn missing_secret_is_a_build_error_naming_only_the_reference() {
    let definition = s3_def(json!({
        "bucket": "artifacts",
        "access_key_id_ref": "AK",
        "secret_access_key_ref": "NOT_THERE",
    }));
    let registry = registry(FakeRepo::with([definition.clone()]));
    let err = registry.store_for(definition.id).await.err().unwrap();
    assert!(matches!(err, RegistryError::Build(BuildError::Secret(_))));
    assert!(err.to_string().contains("NOT_THERE"));
    assert_eq!(registry.cached(), 0);
}

#[tokio::test]
async fn must_not_expose_secret_values_when_the_definition_is_invalid() {
    let definition = s3_def(json!({"bucket": "b", "secret_access_key": "AKIA-leaked-value"}));
    let registry = registry(FakeRepo::with([definition.clone()]));
    let err = registry.store_for(definition.id).await.err().unwrap();
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

    let old = registry.store_for(definition.id).await.unwrap();
    registry.invalidate(definition.id);
    assert_eq!(registry.cached(), 0);
    let new = registry.store_for(definition.id).await.unwrap();
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
    let fetched = registry.store_for(definition.id).await.unwrap();
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
    let store = registry.store_for(definition.id).await.unwrap();

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
