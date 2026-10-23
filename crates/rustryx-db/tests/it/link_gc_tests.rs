//! Blob rows, `AssetRepo::link` and the garbage collection order (row first, object second).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use rustryx_core::StoreError;
use rustryx_core::audit::ActorRef;
use rustryx_core::error::{MutationError, PolicyViolation};
use rustryx_core::events::{EventDraft, EventKind};
use rustryx_core::ids::{BlobId, BlobStoreId};
use rustryx_core::model::{BlobRecord, NewAssetLink, PublishMode, WriteOptions, WritePolicy};
use rustryx_core::ports::{
    AssetRepo, BlobRepo, BlobStoreRepo, ComponentRepo, PageRequest, RepositoryRepo,
};
use time::Duration;
use tokio::sync::Barrier;
use tokio::task::JoinSet;

use crate::fixtures::{
    blob, blob_store, hosted_repository, new_asset, new_component, now, seed_repository, unique,
};
use crate::support::TestDb;

fn link_to(repo: rustryx_core::ids::RepositoryId, path: &str, blob: BlobId) -> NewAssetLink {
    NewAssetLink::new(repo, path, "application/octet-stream", blob)
}

/// A blob that has been unreferenced for an hour.
fn old_blob(store: BlobStoreId, size: u64) -> BlobRecord {
    let mut record = blob(store, size);
    record.created_at = now() - Duration::hours(1);
    record
}

async fn second_repository(
    store: &rustryx_db::PgStore,
    blob_store: BlobStoreId,
) -> rustryx_core::model::Repository {
    let repo = hosted_repository(&unique("repo"), blob_store);
    store.repositories.create(&repo).await.unwrap();
    repo
}

#[tokio::test(flavor = "multi_thread")]
async fn insert_reports_duplicates_and_unknown_stores_as_conflicts() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, _repo) = seed_repository(&store).await;
    let record = blob(bs.id, 7);
    store.blobs.insert(&record).await.unwrap();
    assert_eq!(
        store
            .blobs
            .get(record.id)
            .await
            .unwrap()
            .unwrap()
            .size_bytes,
        7
    );

    let err = store.blobs.insert(&record).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "same id: {err:?}");
    let mut same_key = blob(bs.id, 1);
    same_key.key = record.key.clone();
    let err = store.blobs.insert(&same_key).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(ref m) if m == "blob key already exists in the store"),
        "{err:?}"
    );
    let err = store
        .blobs
        .insert(&blob(BlobStoreId::generate(), 1))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(_)),
        "unknown store: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unreferenced_listing_can_be_restricted_to_one_store() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (first, repo) = seed_repository(&store).await;
    let second = blob_store(&unique("other"));
    store.blob_stores.create(&second).await.unwrap();

    let in_first = old_blob(first.id, 1);
    let in_second = old_blob(second.id, 2);
    let referenced = old_blob(first.id, 3);
    let young = blob(first.id, 4);
    for record in [&in_first, &in_second, &referenced, &young] {
        store.blobs.insert(record).await.unwrap();
    }
    store
        .assets
        .link(
            &link_to(repo.id, "kept", referenced.id),
            &WriteOptions::new(),
        )
        .await
        .unwrap();

    let cutoff = now() - Duration::minutes(5);
    let all: HashSet<_> = store
        .blobs
        .list_unreferenced(cutoff, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.id)
        .collect();
    assert_eq!(all, HashSet::from([in_first.id, in_second.id]));
    let only_first = store
        .blobs
        .list_unreferenced_in(first.id, cutoff, 100)
        .await
        .unwrap();
    assert_eq!(only_first.len(), 1);
    assert_eq!(only_first[0].id, in_first.id);
    let only_second = store
        .blobs
        .list_unreferenced_in(second.id, cutoff, 100)
        .await
        .unwrap();
    assert_eq!(only_second[0].id, in_second.id);
    assert!(
        store
            .blobs
            .list_unreferenced_in(BlobStoreId::generate(), cutoff, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .blobs
            .list_unreferenced_in(first.id, cutoff, 1)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sweep_step_deletes_only_what_is_old_and_unreferenced() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let cutoff = now() - Duration::minutes(5);

    let orphan = old_blob(bs.id, 1);
    store.blobs.insert(&orphan).await.unwrap();
    assert!(
        store
            .blobs
            .delete_if_unreferenced(orphan.id, cutoff)
            .await
            .unwrap()
    );
    assert!(
        store.blobs.get(orphan.id).await.unwrap().is_none(),
        "the row is gone"
    );
    assert!(
        !store
            .blobs
            .delete_if_unreferenced(orphan.id, cutoff)
            .await
            .unwrap(),
        "a second pass, or a second node, finds nothing"
    );

    let young = blob(bs.id, 2);
    store.blobs.insert(&young).await.unwrap();
    assert!(
        !store
            .blobs
            .delete_if_unreferenced(young.id, cutoff)
            .await
            .unwrap()
    );
    assert!(store.blobs.get(young.id).await.unwrap().is_some());

    let held = old_blob(bs.id, 3);
    store.blobs.insert(&held).await.unwrap();
    store
        .assets
        .link(&link_to(repo.id, "held", held.id), &WriteOptions::new())
        .await
        .unwrap();
    assert!(
        !store
            .blobs
            .delete_if_unreferenced(held.id, cutoff)
            .await
            .unwrap()
    );
    assert!(store.blobs.get(held.id).await.unwrap().is_some());

    assert!(
        !store
            .blobs
            .delete_if_unreferenced(BlobId::generate(), cutoff)
            .await
            .unwrap(),
        "a missing row is not an error"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replaced_blob_is_too_young_for_the_sweep_however_old_its_row_is() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let first = old_blob(bs.id, 1);
    store
        .assets
        .publish(&new_asset(repo.id, "p", first.clone()))
        .await
        .unwrap();
    store
        .assets
        .publish(&new_asset(repo.id, "p", blob(bs.id, 2)))
        .await
        .unwrap();
    // `first` was created an hour ago but lost its reference a moment ago.
    let cutoff = now() - Duration::minutes(5);
    assert!(
        !store
            .blobs
            .delete_if_unreferenced(first.id, cutoff)
            .await
            .unwrap()
    );
    assert!(
        store
            .blobs
            .delete_if_unreferenced(first.id, now() + Duration::minutes(1))
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_after_the_mark_wins_over_the_sweep() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let record = old_blob(bs.id, 5);
    store
        .assets
        .publish(&new_asset(repo.id, "gone", record.clone()))
        .await
        .unwrap();
    // The last reference goes away: the blob is marked as an orphan.
    store.assets.delete_by_path(repo.id, "gone").await.unwrap();
    let later = now() + Duration::minutes(1);
    assert_eq!(
        store
            .blobs
            .list_unreferenced(later, 10)
            .await
            .unwrap()
            .len(),
        1
    );

    // A promotion links it before the sweep reaches it.
    store
        .assets
        .link(&link_to(repo.id, "back", record.id), &WriteOptions::new())
        .await
        .unwrap();
    assert!(
        !store
            .blobs
            .delete_if_unreferenced(record.id, later)
            .await
            .unwrap()
    );
    assert!(
        store
            .blobs
            .list_unreferenced(later, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn link_creates_replaces_and_reports_the_displaced_blob_only_when_it_is_free() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, source) = seed_repository(&store).await;
    let target = second_repository(&store, bs.id).await;
    let shared = blob(bs.id, 11);
    store
        .assets
        .publish(&new_asset(source.id, "src", shared.clone()))
        .await
        .unwrap();

    let component = store
        .components
        .find_or_create(&new_component(target.id, None, "lib", Some("1")))
        .await
        .unwrap();
    let mut request = link_to(target.id, "dst", shared.id);
    request.component_id = Some(component.id);
    request.created_by = Some(crate::fixtures::user(&store).await);
    let linked = store
        .assets
        .link(&request, &WriteOptions::new())
        .await
        .unwrap();
    assert!(linked.created && linked.replaced.is_none());
    assert_eq!(linked.asset.blob_id, shared.id);
    assert_eq!(linked.asset.size_bytes, 11);
    assert_eq!(linked.asset.component_id, Some(component.id));
    assert_eq!(linked.asset.created_by, request.created_by);

    // Linking over a path that holds a private blob displaces it, and it is free.
    let private = blob(bs.id, 3);
    store
        .assets
        .publish(&new_asset(target.id, "dst2", private.clone()))
        .await
        .unwrap();
    let over = store
        .assets
        .link(&link_to(target.id, "dst2", shared.id), &WriteOptions::new())
        .await
        .unwrap();
    assert!(!over.created);
    assert_eq!(over.replaced.unwrap().id, private.id);

    // Re-linking the same blob to the same path displaces nothing: the blob it "replaces" is
    // the one it points at, and handing it out for GC would invite deleting a live object.
    let again = store
        .assets
        .link(&link_to(target.id, "dst", shared.id), &WriteOptions::new())
        .await
        .unwrap();
    assert!(!again.created);
    assert!(again.replaced.is_none());

    // Replacing a path whose blob is still shared reports nothing to collect.
    let replaced_shared = store
        .assets
        .publish_with(
            &new_asset(target.id, "dst2", blob(bs.id, 4)),
            &WriteOptions::new(),
        )
        .await
        .unwrap();
    assert!(
        replaced_shared.replaced.is_none(),
        "still referenced by `src` and `dst`"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn link_refuses_a_missing_blob_a_foreign_store_and_a_missing_repository() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let other_store = blob_store(&unique("other"));
    store.blob_stores.create(&other_store).await.unwrap();
    let foreign = blob(other_store.id, 1);
    store.blobs.insert(&foreign).await.unwrap();

    let err = store
        .assets
        .link(
            &link_to(repo.id, "x", BlobId::generate()),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, MutationError::Store(StoreError::NotFound(_))),
        "{err:?}"
    );
    let err = store
        .assets
        .link(&link_to(repo.id, "x", foreign.id), &WriteOptions::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, MutationError::Store(StoreError::Conflict(_))),
        "{err:?}"
    );
    assert!(
        store
            .assets
            .get_by_path(repo.id, "x")
            .await
            .unwrap()
            .is_none()
    );

    let present = blob(bs.id, 1);
    store.blobs.insert(&present).await.unwrap();
    let err = store
        .assets
        .link(
            &link_to(rustryx_core::ids::RepositoryId::generate(), "x", present.id),
            &WriteOptions::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, MutationError::Store(StoreError::NotFound(_))),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn link_respects_the_write_policy_the_mode_and_the_quota() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, mut repo) = seed_repository(&store).await;
    let record = blob(bs.id, 60);
    store.blobs.insert(&record).await.unwrap();
    store
        .assets
        .link(&link_to(repo.id, "a", record.id), &WriteOptions::new())
        .await
        .unwrap();

    // Create only: an existing path is refused and nothing changes.
    let other = blob(bs.id, 5);
    store.blobs.insert(&other).await.unwrap();
    let err = store
        .assets
        .link(
            &link_to(repo.id, "a", other.id),
            &WriteOptions::new().with_mode(PublishMode::CreateOnly),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            MutationError::Violation(PolicyViolation::AlreadyExists)
        ),
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
        record.id
    );

    // Quota: a second path to the SAME blob costs nothing (distinct blobs), a new blob does.
    repo.quota_bytes = Some(100);
    store.repositories.update(&repo).await.unwrap();
    store
        .assets
        .link(&link_to(repo.id, "b", record.id), &WriteOptions::new())
        .await
        .unwrap();
    assert_eq!(
        store.blobs.used_bytes_by_repository(repo.id).await.unwrap(),
        60
    );
    let big = blob(bs.id, 41);
    store.blobs.insert(&big).await.unwrap();
    let err = store
        .assets
        .link(&link_to(repo.id, "c", big.id), &WriteOptions::new())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            MutationError::Violation(PolicyViolation::QuotaExceeded(_))
        ),
        "{err:?}"
    );
    assert!(
        store
            .assets
            .get_by_path(repo.id, "c")
            .await
            .unwrap()
            .is_none()
    );

    repo.write_policy = WritePolicy::Deny;
    store.repositories.update(&repo).await.unwrap();
    let err = store
        .assets
        .link(&link_to(repo.id, "d", other.id), &WriteOptions::new())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            MutationError::Violation(PolicyViolation::WritePolicy(WritePolicy::Deny))
        ),
        "{err:?}"
    );
}

/// The point of the whole change: the object of a blob is deleted only after its row went,
/// and a link that succeeded is never left pointing at a deleted object.
///
/// The "object store" is a set of blob ids. The sweep removes an id from it only after
/// `delete_if_unreferenced` returned `true`, exactly as the GC job does. A link either wins
/// (the blob row stays, the sweep reports `false`, the object stays) or loses (`NotFound`,
/// nothing was created). Both outcomes race on one barrier, over many rounds.
#[tokio::test(flavor = "multi_thread")]
async fn must_not_leave_a_link_pointing_at_a_deleted_object_under_concurrent_gc() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    let objects: Arc<Mutex<HashSet<BlobId>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut links_won = 0;
    let mut sweeps_won = 0;

    for round in 0..40_u64 {
        let record = old_blob(bs.id, 1);
        store.blobs.insert(&record).await.unwrap();
        objects.lock().unwrap().insert(record.id);
        let linkers = 3;
        let barrier = Arc::new(Barrier::new(linkers + 1));
        let mut tasks: JoinSet<Result<bool, StoreError>> = JoinSet::new();
        for n in 0..linkers {
            let assets = store.assets.clone();
            let request = link_to(repo.id, &format!("r{round}/l{n}"), record.id);
            let barrier = Arc::clone(&barrier);
            tasks.spawn(async move {
                barrier.wait().await;
                match assets.link(&request, &WriteOptions::new()).await {
                    Ok(_) => Ok(true),
                    Err(MutationError::Store(StoreError::NotFound(_))) => Ok(false),
                    Err(MutationError::Store(other)) => Err(other),
                    Err(other) => panic!("unexpected {other:?}"),
                }
            });
        }
        let sweep_store = store.clone();
        let sweep_objects = Arc::clone(&objects);
        let sweep_barrier = Arc::clone(&barrier);
        let blob_id = record.id;
        // The sweep starts a little later each round, so that over the rounds it lands before,
        // inside and after the transactions of the links instead of always winning the race.
        let head_start = std::time::Duration::from_micros(round * 100);
        let sweep = tokio::spawn(async move {
            sweep_barrier.wait().await;
            tokio::time::sleep(head_start).await;
            let deleted = sweep_store
                .blobs
                .delete_if_unreferenced(blob_id, now() + Duration::minutes(1))
                .await
                .unwrap();
            if deleted {
                // Row first, object second.
                sweep_objects.lock().unwrap().remove(&blob_id);
            }
            deleted
        });

        let mut linked = 0;
        while let Some(done) = tasks.join_next().await {
            if done.unwrap().unwrap() {
                linked += 1;
            }
        }
        let swept = sweep.await.unwrap();

        if swept {
            sweeps_won += 1;
            assert_eq!(
                linked, 0,
                "round {round}: a link succeeded on a blob the sweep deleted"
            );
            assert!(store.blobs.get(record.id).await.unwrap().is_none());
        } else {
            links_won += 1;
            assert!(linked > 0, "round {round}: neither side won");
            assert!(
                store.blobs.get(record.id).await.unwrap().is_some(),
                "round {round}"
            );
            assert!(
                objects.lock().unwrap().contains(&record.id),
                "round {round}: the object of a linked blob was deleted"
            );
        }
        // Whatever happened, every asset of the round points at a blob whose object exists.
        let assets = store
            .assets
            .list(repo.id, &format!("r{round}/"), &PageRequest::default())
            .await
            .unwrap();
        for asset in assets.items {
            assert!(
                objects.lock().unwrap().contains(&asset.blob_id),
                "round {round}"
            );
        }
    }
    // Both orders happen in practice; the test asserts safety, not the split.
    assert_eq!(links_won + sweeps_won, 40);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_links_of_one_orphaned_blob_do_not_deadlock() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, repo) = seed_repository(&store).await;
    // The blob is an orphan (its `orphaned_at` is set), so the reference trigger must touch
    // its row: the situation in which two share locks would deadlock.
    let record = old_blob(bs.id, 1);
    store
        .assets
        .publish(&new_asset(repo.id, "seed", record.clone()))
        .await
        .unwrap();
    store.assets.delete_by_path(repo.id, "seed").await.unwrap();
    let mut tasks = JoinSet::new();
    for n in 0..8 {
        let assets = store.assets.clone();
        let request = link_to(repo.id, &format!("l{n}"), record.id);
        tasks.spawn(async move { assets.link(&request, &WriteOptions::new()).await });
    }
    while let Some(done) = tasks.join_next().await {
        done.unwrap().unwrap();
    }
    assert_eq!(
        store
            .assets
            .list(repo.id, "l", &PageRequest::default())
            .await
            .unwrap()
            .items
            .len(),
        8
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_by_path_checked_reports_the_freed_blob_and_writes_events_only_for_a_delete() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let (bs, source) = seed_repository(&store).await;
    let target = second_repository(&store, bs.id).await;
    let shared = blob(bs.id, 8);
    store
        .assets
        .publish(&new_asset(source.id, "a", shared.clone()))
        .await
        .unwrap();
    store
        .assets
        .link(&link_to(target.id, "b", shared.id), &WriteOptions::new())
        .await
        .unwrap();

    let mut draft = EventDraft::new(
        EventKind::Delete,
        ActorRef::system("test"),
        serde_json::json!({}),
    );
    draft.repository_id = Some(source.id);
    let options = WriteOptions::new().with_event(draft);

    // The first delete leaves the blob referenced by the other repository: nothing to collect.
    assert!(
        store
            .assets
            .delete_by_path_checked(source.id, "a", &options)
            .await
            .unwrap()
            .is_none()
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(events, 1);
    // A missing path is idempotent and silent.
    assert!(
        store
            .assets
            .delete_by_path_checked(source.id, "a", &options)
            .await
            .unwrap()
            .is_none()
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(events, 1, "nothing was deleted, nothing is announced");
    // The last reference frees the blob.
    let freed = store
        .assets
        .delete_by_path_checked(target.id, "b", &WriteOptions::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(freed.id, shared.id);
}
