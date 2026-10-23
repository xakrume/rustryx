use std::collections::HashSet;

use rustryx_core::StoreError;
use rustryx_core::ids::{AssetId, BlobId, ComponentId, RepositoryId};
use rustryx_core::ports::{AssetRepo, BlobRepo, ComponentRepo, PageRequest, RepositoryRepo};
use time::Duration;
use tokio::task::JoinSet;

use crate::fixtures::{blob, new_asset, new_component, now, publish, seed_repository};
use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn publish_new_path_returns_asset_without_replaced_blob() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let record = blob(bs.id, 1234);
    let outcome = store
        .assets
        .publish(&new_asset(
            repo.id,
            "com/acme/app/1.0/app-1.0.jar",
            record.clone(),
        ))
        .await
        .unwrap();
    assert!(outcome.replaced.is_none());
    let asset = outcome.asset;
    assert_eq!(asset.path, "com/acme/app/1.0/app-1.0.jar");
    assert_eq!(asset.blob_id, record.id);
    assert_eq!(asset.size_bytes, 1234);
    assert_eq!(asset.checksums, record.checksums);
    assert_eq!(asset.content_type, "application/octet-stream");
    assert!(asset.last_downloaded_at.is_none());

    let by_path = store
        .assets
        .get_by_path(repo.id, &asset.path)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_path.id, asset.id);
    assert_eq!(
        store.assets.get(asset.id).await.unwrap().unwrap().path,
        asset.path
    );
    assert!(
        store
            .assets
            .get_by_path(repo.id, "missing")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .assets
            .get(AssetId::generate())
            .await
            .unwrap()
            .is_none()
    );
    let stored_blob = store.blobs.get(record.id).await.unwrap().unwrap();
    assert_eq!(stored_blob.key, record.key);
    assert_eq!(stored_blob.size_bytes, 1234);
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_over_existing_path_switches_blob_and_returns_the_old_one() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let first_blob = blob(bs.id, 10);
    let first = store
        .assets
        .publish(&new_asset(repo.id, "file.bin", first_blob.clone()))
        .await
        .unwrap();
    let second_blob = blob(bs.id, 20);
    let second = store
        .assets
        .publish(&new_asset(repo.id, "file.bin", second_blob.clone()))
        .await
        .unwrap();

    assert_eq!(second.replaced.as_ref().unwrap().id, first_blob.id);
    assert_eq!(
        second.asset.id, first.asset.id,
        "same path keeps its asset id"
    );
    assert_eq!(second.asset.blob_id, second_blob.id);
    assert_eq!(second.asset.size_bytes, 20);
    assert_eq!(second.asset.created_at, first.asset.created_at);
    assert!(second.asset.updated_at >= first.asset.updated_at);
    // The old blob row survives until GC deletes it.
    assert!(store.blobs.get(first_blob.id).await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_leave_a_blob_row_when_the_asset_insert_fails() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _repo) = seed_repository(&store).await;
    let record = blob(bs.id, 5);
    // The repository does not exist: the asset insert violates its foreign key after the
    // blob row was inserted in the same transaction.
    let err = store
        .assets
        .publish(&new_asset(RepositoryId::generate(), "x", record.clone()))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    assert!(store.blobs.get(record.id).await.unwrap().is_none());
    let blobs: i64 = sqlx::query_scalar("SELECT count(*) FROM blobs")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(blobs, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_blob_key_conflicts_and_keeps_the_current_asset() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let first = blob(bs.id, 5);
    store
        .assets
        .publish(&new_asset(repo.id, "a", first.clone()))
        .await
        .unwrap();
    let mut clash = blob(bs.id, 6);
    clash.key = first.key.clone();
    let err = store
        .assets
        .publish(&new_asset(repo.id, "a", clash))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "blob key already exists in the store"),
        "{err:?}"
    );
    assert_eq!(
        store
            .assets
            .get_by_path(repo.id, "a")
            .await
            .unwrap()
            .unwrap()
            .blob_id,
        first.id
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_publish_of_one_path_leaves_one_asset_and_accounts_for_every_blob() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;

    let mut tasks = JoinSet::new();
    let mut blob_ids = HashSet::new();
    for i in 0..16_u64 {
        let record = blob(bs.id, 100 + i);
        blob_ids.insert(record.id);
        let assets = store.assets.clone();
        let new = new_asset(repo.id, "race/target.bin", record);
        tasks.spawn(async move { assets.publish(&new).await });
    }
    let mut replaced: Vec<BlobId> = Vec::new();
    let mut inserts = 0;
    let mut final_ids = HashSet::new();
    while let Some(joined) = tasks.join_next().await {
        let outcome = joined.unwrap().unwrap();
        final_ids.insert(outcome.asset.id);
        match outcome.replaced {
            Some(old) => replaced.push(old.id),
            None => inserts += 1,
        }
    }
    assert_eq!(inserts, 1, "exactly one publisher created the path");
    assert_eq!(final_ids.len(), 1, "every publisher saw the same asset row");
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM assets WHERE path = 'race/target.bin'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(rows, 1);

    let current = store
        .assets
        .get_by_path(repo.id, "race/target.bin")
        .await
        .unwrap()
        .unwrap()
        .blob_id;
    let replaced_set: HashSet<BlobId> = replaced.iter().copied().collect();
    assert_eq!(
        replaced.len(),
        replaced_set.len(),
        "no blob is reported twice"
    );
    assert!(!replaced_set.contains(&current));
    let mut expected = blob_ids.clone();
    expected.remove(&current);
    assert_eq!(
        replaced_set, expected,
        "every displaced blob was reported exactly once"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_uses_keyset_pagination_by_path_and_escapes_the_prefix() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    for path in [
        "a_b/1", "a_b/2", "axb/1", "a%b/1", "z/1", "a/1", "a/2", "a/3",
    ] {
        publish(&store, repo.id, bs.id, path).await;
    }

    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = store
            .assets
            .list(
                repo.id,
                "",
                &PageRequest {
                    limit: 3,
                    after: after.clone(),
                },
            )
            .await
            .unwrap();
        pages += 1;
        seen.extend(page.items.iter().map(|a| a.path.clone()));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "byte order");
    assert_eq!(seen.len(), 8);

    // `_` and `%` are literals in a prefix, not wildcards.
    let underscore = store
        .assets
        .list(repo.id, "a_b", &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(underscore.items.len(), 2);
    let percent = store
        .assets
        .list(repo.id, "a%", &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(percent.items.len(), 1);
    let plain = store
        .assets
        .list(repo.id, "a/", &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(plain.items.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_accept_a_forged_page_token() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (_bs, repo) = seed_repository(&store).await;
    for bad in ["not base64!!", "bm9wZQ", ""] {
        let err = store
            .assets
            .list(
                repo.id,
                "",
                &PageRequest {
                    limit: 10,
                    after: Some(bad.to_owned()),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref m) if m == "invalid page token"),
            "{bad}: {err:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn list_by_component_returns_only_its_assets() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let component = store
        .components
        .find_or_create(&new_component(
            repo.id,
            Some("com.acme"),
            "app",
            Some("1.0"),
        ))
        .await
        .unwrap();
    for path in ["app-1.0.jar", "app-1.0.pom"] {
        let mut new = new_asset(repo.id, path, blob(bs.id, 1));
        new.component_id = Some(component.id);
        store.assets.publish(&new).await.unwrap();
    }
    publish(&store, repo.id, bs.id, "unrelated").await;
    let assets = store.assets.list_by_component(component.id).await.unwrap();
    assert_eq!(assets.len(), 2);
    assert!(assets.iter().all(|a| a.component_id == Some(component.id)));
    assert!(
        store
            .assets
            .list_by_component(ComponentId::generate())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_by_path_returns_the_blob_that_lost_its_last_reference() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let record = blob(bs.id, 7);
    store
        .assets
        .publish(&new_asset(repo.id, "gone", record.clone()))
        .await
        .unwrap();

    let freed = store
        .assets
        .delete_by_path(repo.id, "gone")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(freed.id, record.id);
    assert!(
        store
            .assets
            .get_by_path(repo.id, "gone")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .assets
            .delete_by_path(repo.id, "gone")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_by_path_keeps_a_blob_that_another_asset_still_uses() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let record = blob(bs.id, 7);
    store
        .assets
        .publish(&new_asset(repo.id, "one", record.clone()))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO assets (id, repository_id, path, blob_id, content_type) VALUES ($1, $2, 'two', $3, 'x')",
    )
    .bind(AssetId::generate().as_uuid())
    .bind(repo.id.as_uuid())
    .bind(record.id.as_uuid())
    .execute(store.pool())
    .await
    .unwrap();

    assert!(
        store
            .assets
            .delete_by_path(repo.id, "one")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.blobs.used_bytes_by_repository(repo.id).await.unwrap(),
        7
    );
    let freed = store
        .assets
        .delete_by_path(repo.id, "two")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(freed.id, record.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn touch_downloaded_batches_and_never_moves_time_backwards() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let a = publish(&store, repo.id, bs.id, "a").await;
    let b = publish(&store, repo.id, bs.id, "b").await;
    store.assets.touch_downloaded(&[]).await.unwrap();

    let t0 = now();
    let t1 = t0 + Duration::seconds(10);
    // Duplicate ids in one batch: the newest time wins whatever the order.
    store
        .assets
        .touch_downloaded(&[(a, t1), (a, t0), (b, t0), (a, t0 + Duration::seconds(5))])
        .await
        .unwrap();
    assert_eq!(
        store
            .assets
            .get(a)
            .await
            .unwrap()
            .unwrap()
            .last_downloaded_at,
        Some(t1)
    );
    assert_eq!(
        store
            .assets
            .get(b)
            .await
            .unwrap()
            .unwrap()
            .last_downloaded_at,
        Some(t0)
    );

    store
        .assets
        .touch_downloaded(&[(a, t0 - Duration::hours(1))])
        .await
        .unwrap();
    assert_eq!(
        store
            .assets
            .get(a)
            .await
            .unwrap()
            .unwrap()
            .last_downloaded_at,
        Some(t1)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_list_a_recently_replaced_blob_for_garbage_collection() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    // An old blob, replaced just now: its own age says "collect", the replacement says "wait".
    let mut old = blob(bs.id, 10);
    old.created_at = now() - Duration::days(30);
    store
        .assets
        .publish(&new_asset(repo.id, "f", old.clone()))
        .await
        .unwrap();
    store
        .assets
        .publish(&new_asset(repo.id, "f", blob(bs.id, 11)))
        .await
        .unwrap();

    let recent = store
        .blobs
        .list_unreferenced(now() - Duration::hours(1), 100)
        .await
        .unwrap();
    assert!(recent.is_empty(), "{recent:?}");
    let later = store
        .blobs
        .list_unreferenced(now() + Duration::seconds(5), 100)
        .await
        .unwrap();
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].id, old.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn gc_flow_orders_oldest_first_limits_and_deletes_idempotently() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    for path in ["a", "b", "c"] {
        publish(&store, repo.id, bs.id, path).await;
        store.assets.delete_by_path(repo.id, path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let horizon = now() + Duration::seconds(5);
    let all = store.blobs.list_unreferenced(horizon, 100).await.unwrap();
    assert_eq!(all.len(), 3);
    let limited = store.blobs.list_unreferenced(horizon, 2).await.unwrap();
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].id, all[0].id);

    store.blobs.delete(all[0].id).await.unwrap();
    store.blobs.delete(all[0].id).await.unwrap();
    assert_eq!(
        store
            .blobs
            .list_unreferenced(horizon, 100)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_referenced_blob_cannot_be_deleted() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let record = blob(bs.id, 3);
    store
        .assets
        .publish(&new_asset(repo.id, "kept", record.clone()))
        .await
        .unwrap();
    let err = store.blobs.delete(record.id).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "blob is still referenced by an asset"),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_counts_distinct_blobs_per_repository_and_all_blobs_per_store() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    assert_eq!(
        store.blobs.used_bytes_by_repository(repo.id).await.unwrap(),
        0
    );
    let shared = blob(bs.id, 100);
    store
        .assets
        .publish(&new_asset(repo.id, "x", shared.clone()))
        .await
        .unwrap();
    sqlx::query("INSERT INTO assets (id, repository_id, path, blob_id, content_type) VALUES ($1, $2, 'y', $3, 'x')")
        .bind(AssetId::generate().as_uuid())
        .bind(repo.id.as_uuid())
        .bind(shared.id.as_uuid())
        .execute(store.pool())
        .await
        .unwrap();
    store
        .assets
        .publish(&new_asset(repo.id, "z", blob(bs.id, 50)))
        .await
        .unwrap();
    assert_eq!(
        store.blobs.used_bytes_by_repository(repo.id).await.unwrap(),
        150
    );
    assert_eq!(store.blobs.used_bytes_by_store(bs.id).await.unwrap(), 150);
    // A replaced blob still occupies the store until GC removes it.
    store
        .assets
        .publish(&new_asset(repo.id, "z", blob(bs.id, 25)))
        .await
        .unwrap();
    assert_eq!(
        store.blobs.used_bytes_by_repository(repo.id).await.unwrap(),
        125
    );
    assert_eq!(store.blobs.used_bytes_by_store(bs.id).await.unwrap(), 175);
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_repository_cascades_and_frees_its_blobs_for_gc() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    publish(&store, repo.id, bs.id, "p1").await;
    publish(&store, repo.id, bs.id, "p2").await;
    store
        .components
        .find_or_create(&new_component(repo.id, None, "c", Some("1")))
        .await
        .unwrap();
    store.repositories.delete(repo.id).await.unwrap();

    let assets: i64 = sqlx::query_scalar("SELECT count(*) FROM assets")
        .fetch_one(store.pool())
        .await
        .unwrap();
    let components: i64 = sqlx::query_scalar("SELECT count(*) FROM components")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!((assets, components), (0, 0));
    let freed = store
        .blobs
        .list_unreferenced(now() + Duration::seconds(5), 100)
        .await
        .unwrap();
    assert_eq!(freed.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_checksums_must_be_lowercase_hex() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let mut bad = blob(bs.id, 1);
    bad.checksums.sha256 = "XYZ".to_owned();
    let err = store
        .assets
        .publish(&new_asset(repo.id, "bad", bad))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
}
