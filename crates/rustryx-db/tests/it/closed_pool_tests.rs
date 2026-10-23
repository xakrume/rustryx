//! A database that cannot be reached is `Unavailable` from every port method, and never a
//! panic, a hang or a different error. One test walks the methods that carry the policy and
//! the foundation ports, over a pool that was closed after the store was built.

use std::fmt::Debug;
use std::time::Duration;

use futures::StreamExt;
use rustryx_core::StoreError;
use rustryx_core::error::MutationError;
use rustryx_core::events::{CursorAdvance, OutboxCursor};
use rustryx_core::ids::{AssetId, BlobStoreId, ComponentId, RepositoryId, RoutingRuleId, UserId};
use rustryx_core::model::{DownloadTick, NewAssetLink, WriteOptions};
use rustryx_core::ports::{
    AssetRepo, AuditQuery, AuditReadRepo, AuditSink, BlobRepo, BlobStoreRepo, ComponentFilter,
    ComponentRepo, JobLeaseRepo, NewJob, PageRequest, RepositoryRepo, RevokedTokenRepo,
    RoutingRuleRepo, SecretView, SystemConfigRepo, UserRepo,
};
use rustryx_core::routing::{RoutingMode, RoutingRule};
use uuid::Uuid;

use crate::fixtures::{blob, blob_store, hosted_repository, new_asset, new_component, now};
use crate::support::{TestDb, test_sealer};

fn unavailable<T: Debug>(what: &str, result: Result<T, StoreError>) {
    match result {
        Err(StoreError::Unavailable(_)) => {}
        other => panic!("{what}: expected Unavailable, got {other:?}"),
    }
}

fn unavailable_mutation<T: Debug>(what: &str, result: Result<T, MutationError>) {
    match result {
        Err(MutationError::Store(StoreError::Unavailable(_))) => {}
        other => panic!("{what}: expected Unavailable, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_port_reports_a_closed_pool_as_unavailable() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let store = rustryx_db::PgStore::new(pool.clone(), test_sealer());
    pool.close().await;

    let repo_id = RepositoryId::generate();
    let store_id = BlobStoreId::generate();
    let record = blob(store_id, 1);
    let asset_id = AssetId::generate();
    let page = PageRequest::default();
    let write = WriteOptions::new();

    // Repositories and blob stores.
    let repository = hosted_repository("closed-repo", store_id);
    unavailable("repo create", store.repositories.create(&repository).await);
    unavailable("repo get", store.repositories.get(repo_id).await);
    unavailable("repo list", store.repositories.list().await);
    unavailable("repo update", store.repositories.update(&repository).await);
    unavailable("repo delete", store.repositories.delete(repo_id).await);
    let def = blob_store("closed-store");
    unavailable("store create", store.blob_stores.create(&def).await);
    unavailable("store get", store.blob_stores.get(store_id).await);
    unavailable("store by name", store.blob_stores.get_by_name("x").await);
    unavailable("store list", store.blob_stores.list().await);
    unavailable("store update", store.blob_stores.update(&def).await);
    unavailable("store delete", store.blob_stores.delete(store_id).await);

    // Blobs.
    unavailable("blob get", store.blobs.get(record.id).await);
    unavailable("blob insert", store.blobs.insert(&record).await);
    unavailable(
        "blob usage",
        store.blobs.used_bytes_by_repository(repo_id).await,
    );
    unavailable(
        "blob store usage",
        store.blobs.used_bytes_by_store(store_id).await,
    );
    unavailable("blob list", store.blobs.list_unreferenced(now(), 10).await);
    unavailable(
        "blob list in",
        store.blobs.list_unreferenced_in(store_id, now(), 10).await,
    );
    unavailable("blob delete", store.blobs.delete(record.id).await);
    unavailable(
        "blob sweep",
        store.blobs.delete_if_unreferenced(record.id, now()).await,
    );

    // Components.
    let new = new_component(repo_id, None, "c", Some("1"));
    unavailable(
        "component find or create",
        store.components.find_or_create(&new).await,
    );
    unavailable(
        "component get",
        store.components.get(ComponentId::generate()).await,
    );
    unavailable("component find", store.components.find(&new).await);
    unavailable(
        "component list",
        store
            .components
            .list(&ComponentFilter::default(), &page)
            .await,
    );
    unavailable(
        "component delete",
        store.components.delete(ComponentId::generate()).await,
    );
    unavailable_mutation(
        "component delete checked",
        store
            .components
            .delete_checked(ComponentId::generate(), &write)
            .await,
    );

    // Assets.
    unavailable(
        "asset by path",
        store.assets.get_by_path(repo_id, "p").await,
    );
    unavailable("asset get", store.assets.get(asset_id).await);
    unavailable_mutation(
        "asset publish with",
        store
            .assets
            .publish_with(&new_asset(repo_id, "p", record.clone()), &write)
            .await,
    );
    unavailable(
        "asset publish",
        store
            .assets
            .publish(&new_asset(repo_id, "p", record.clone()))
            .await,
    );
    unavailable_mutation(
        "asset link",
        store
            .assets
            .link(&NewAssetLink::new(repo_id, "p", "x/y", record.id), &write)
            .await,
    );
    unavailable("asset list", store.assets.list(repo_id, "", &page).await);
    unavailable(
        "asset list by component",
        store
            .assets
            .list_by_component(ComponentId::generate())
            .await,
    );
    unavailable(
        "asset delete",
        store.assets.delete_by_path(repo_id, "p").await,
    );
    unavailable_mutation(
        "asset delete checked",
        store
            .assets
            .delete_by_path_checked(repo_id, "p", &write)
            .await,
    );
    unavailable(
        "asset touch downloaded",
        store.assets.touch_downloaded(&[(asset_id, now())]).await,
    );
    unavailable(
        "asset touch fresh",
        store.assets.touch_fresh(asset_id, now()).await,
    );
    unavailable(
        "asset record downloads",
        store
            .assets
            .record_downloads(&[DownloadTick {
                asset_id,
                at: now(),
                count: 1,
            }])
            .await,
    );

    // Users.
    unavailable(
        "user compare and set",
        store
            .users
            .set_password_hash_if(UserId::generate(), "old", "new")
            .await,
    );

    // Audit, queues, revocations, rules, settings.
    unavailable(
        "audit record",
        store
            .audit
            .record(rustryx_core::audit::AuditEvent::new(
                rustryx_core::audit::AuditKind::LoginSuccess,
                "x",
                "a",
                "success",
            ))
            .await,
    );
    unavailable(
        "audit query",
        store.audit_read.query(&AuditQuery::new(), &page).await,
    );
    let first = store
        .audit_read
        .export(&AuditQuery::new())
        .next()
        .await
        .unwrap();
    unavailable("audit export", first);

    let leases = &store.job_leases;
    let payload = serde_json::json!({});
    unavailable("enqueue", leases.enqueue("q", None, &payload).await);
    unavailable(
        "enqueue after",
        leases
            .enqueue_after("q", None, &payload, Duration::ZERO)
            .await,
    );
    unavailable(
        "claim",
        leases.claim("q", "n", Duration::from_secs(1)).await,
    );
    unavailable(
        "heartbeat",
        leases
            .heartbeat(Uuid::now_v7(), "n", Duration::from_secs(1))
            .await,
    );
    unavailable("complete", leases.complete(Uuid::now_v7(), "n").await);
    unavailable(
        "release",
        leases
            .release_for_retry(Uuid::now_v7(), "n", Duration::ZERO, "e")
            .await,
    );
    unavailable("fail", leases.fail(Uuid::now_v7(), "n", "e").await);
    let one = NewJob {
        queue: "q".to_owned(),
        dedupe_key: None,
        payload: payload.clone(),
        delay: Duration::ZERO,
    };
    unavailable(
        "batch",
        leases.enqueue_batch(std::slice::from_ref(&one), None).await,
    );
    let advance = CursorAdvance {
        consumer: "scan",
        from: OutboxCursor::default(),
        to: OutboxCursor { tx_id: 1, seq: 1 },
    };
    unavailable(
        "batch cursor",
        leases.enqueue_batch(&[], Some(&advance)).await,
    );
    unavailable("depth", leases.depth("q").await);
    unavailable("purge finished", leases.purge_finished(now()).await);

    let tid = Uuid::now_v7();
    unavailable("revoke", store.revoked_tokens.revoke(tid, now()).await);
    unavailable("is revoked", store.revoked_tokens.is_revoked(tid).await);
    unavailable(
        "purge revoked",
        store.revoked_tokens.purge_expired(now()).await,
    );

    let rule = RoutingRule::new(
        RoutingRuleId::generate(),
        "r",
        RoutingMode::Block,
        vec!["a".to_owned()],
        now(),
    );
    let rules = &store.routing_rules;
    unavailable("rule create", rules.create(&rule).await);
    unavailable("rule get", rules.get(rule.id).await);
    unavailable("rule list", rules.list().await);
    unavailable("rule update", rules.update(&rule, 1).await);
    unavailable("rule delete", rules.delete(rule.id).await);
    unavailable("rule assigned", rules.assigned_to(rule.id).await);

    let config = &store.system_config;
    unavailable("config get", config.get("k", SecretView::Mask).await);
    unavailable(
        "config list",
        config.list_prefix("k", SecretView::Reveal).await,
    );
    unavailable(
        "config put",
        config.put("k", &payload, &[], None, None).await,
    );
    unavailable("config delete", config.delete("k", None).await);
    unavailable("config revision", config.current_revision().await);
}
