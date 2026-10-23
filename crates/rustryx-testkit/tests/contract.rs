//! The doubles must be usable the way the composition root uses the real adapters: as
//! shared trait objects.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure signal"
)]

use std::sync::Arc;

use rustryx_core::audit::{AuditEvent, AuditKind};
use rustryx_core::ports::{
    AssetRepo, AuditSink, BlobRepo, BlobStore, BlobStoreRepo, Clock, ComponentRepo, IdpMappingRepo,
    InvalidationBus, InvalidationEvent, LockBackend, LoginAttemptRepo, PrivilegeRepo,
    RepositoryRepo, RoleRepo, SelectorRepo, TokenRepo, UserRepo,
};
use rustryx_testkit::{
    FixedClock, MemAuditSink, MemBlobStore, MemInvalidationBus, MemLockBackend, MemStore,
};
use time::OffsetDateTime;

fn assert_shareable<T: Send + Sync + 'static>() {}

#[test]
fn every_double_is_send_sync_and_static() {
    assert_shareable::<MemStore>();
    assert_shareable::<MemBlobStore>();
    assert_shareable::<MemLockBackend>();
    assert_shareable::<MemAuditSink>();
    assert_shareable::<MemInvalidationBus>();
    assert_shareable::<FixedClock>();
}

#[tokio::test]
async fn one_mem_store_serves_every_metadata_port_as_a_trait_object() {
    let store = MemStore::with_builtin_roles().with_default_blob_store();
    let repositories: Arc<dyn RepositoryRepo> = Arc::new(store.clone());
    let blob_stores: Arc<dyn BlobStoreRepo> = Arc::new(store.clone());
    let blobs: Arc<dyn BlobRepo> = Arc::new(store.clone());
    let components: Arc<dyn ComponentRepo> = Arc::new(store.clone());
    let assets: Arc<dyn AssetRepo> = Arc::new(store.clone());
    let users: Arc<dyn UserRepo> = Arc::new(store.clone());
    let roles: Arc<dyn RoleRepo> = Arc::new(store.clone());
    let privileges: Arc<dyn PrivilegeRepo> = Arc::new(store.clone());
    let selectors: Arc<dyn SelectorRepo> = Arc::new(store.clone());
    let tokens: Arc<dyn TokenRepo> = Arc::new(store.clone());
    let mappings: Arc<dyn IdpMappingRepo> = Arc::new(store.clone());
    let attempts: Arc<dyn LoginAttemptRepo> = Arc::new(store);

    assert!(repositories.list().await.unwrap().is_empty());
    assert_eq!(blob_stores.list().await.unwrap().len(), 1);
    assert!(
        blobs
            .get(rustryx_core::ids::BlobId::generate())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        components
            .get(rustryx_core::ids::ComponentId::generate())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        assets
            .get(rustryx_core::ids::AssetId::generate())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        users
            .get(rustryx_core::ids::UserId::generate())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(roles.list().await.unwrap().len(), 3);
    assert_eq!(privileges.list().await.unwrap().len(), 3);
    assert!(selectors.list().await.unwrap().is_empty());
    assert!(tokens.find_by_hash("none").await.unwrap().is_none());
    assert!(
        mappings
            .list(rustryx_core::identity::IdpProvider::Ldap)
            .await
            .unwrap()
            .is_empty()
    );
    let key = rustryx_core::ports::LoginKey {
        account: "a".to_owned(),
        ip: None,
    };
    assert_eq!(attempts.state(&key).await.unwrap().failures, 0);
}

#[tokio::test]
async fn infrastructure_doubles_work_as_trait_objects() {
    let blob_store: Arc<dyn BlobStore> = Arc::new(MemBlobStore::new());
    blob_store.health().await.unwrap();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(OffsetDateTime::UNIX_EPOCH));
    assert_eq!(clock.now(), OffsetDateTime::UNIX_EPOCH);
    let locks: Arc<dyn LockBackend> = Arc::new(MemLockBackend::with_clock(Arc::clone(&clock)));
    assert!(
        locks
            .try_acquire("k", std::time::Duration::from_secs(1))
            .await
            .unwrap()
            .is_some()
    );
    let bus: Arc<dyn InvalidationBus> = Arc::new(MemInvalidationBus::new());
    bus.publish(InvalidationEvent::Rbac).await.unwrap();

    let sink = MemAuditSink::new();
    let audit: Arc<dyn AuditSink> = Arc::new(sink.clone());
    audit
        .record(AuditEvent {
            at: clock.now(),
            kind: AuditKind::LoginSuccess,
            actor_id: None,
            actor_name: "alice".to_owned(),
            client_ip: None,
            request_id: None,
            action: "login".to_owned(),
            target: None,
            outcome: "success".to_owned(),
            detail: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert_eq!(sink.events().len(), 1);
}
