//! `MemStore`: repositories, blob stores, blobs, components and assets.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure signal"
)]

use std::sync::Arc;
use std::time::Duration;

use rustryx_core::StoreError;
use rustryx_core::ids::{BlobId, BlobStoreId, ComponentId, RepositoryId};
use rustryx_core::model::{
    BlobStoreKind, Component, NewAsset, NewComponent, RepoFormat, RepoType, Repository, WritePolicy,
};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{
    AssetRepo, BlobRepo, BlobStoreRepo, Clock, ComponentFilter, ComponentRepo, Page, PageRequest,
    RepositoryRepo,
};
use rustryx_testkit::fixtures::{
    BLOB_STORE_ID, blob_record, blob_record_at, blob_store_def, repository,
};
use rustryx_testkit::{FixedClock, MemStore};
use time::OffsetDateTime;

fn hosted(name: &str) -> Repository {
    repository(name, RepoFormat::Raw, RepoType::Hosted)
}

fn new_asset(repo: RepositoryId, path: &str, content: &[u8]) -> NewAsset {
    NewAsset {
        repository_id: repo,
        component_id: None,
        path: path.to_owned(),
        content_type: "application/octet-stream".to_owned(),
        blob: blob_record(&format!("key-{path}"), content),
    }
}

fn coordinate(repo: RepositoryId, name: &str, version: &str) -> NewComponent {
    NewComponent {
        repository_id: repo,
        format: RepoFormat::Maven2,
        group_id: Some("org.example".to_owned()),
        name: name.to_owned(),
        version: Some(version.to_owned()),
    }
}

fn page(limit: u32, after: Option<String>) -> PageRequest {
    PageRequest { limit, after }
}

/// `MemStore` implements several traits with a `list` method, so a plain method call is
/// ambiguous in a file that imports more than one of them.
async fn list_components(
    store: &MemStore,
    filter: &ComponentFilter,
    request: &PageRequest,
) -> Result<Page<Component>, StoreError> {
    ComponentRepo::list(store, filter, request).await
}

// repositories

#[tokio::test]
async fn repository_create_get_list_roundtrip_ordered_by_name() {
    let store = MemStore::new();
    let (b, a) = (hosted("b-repo"), hosted("a-repo"));
    RepositoryRepo::create(&store, &b).await.unwrap();
    RepositoryRepo::create(&store, &a).await.unwrap();
    let by_id = RepositoryRepo::get(&store, a.id).await.unwrap().unwrap();
    assert_eq!(by_id.name, a.name);
    let by_name = RepositoryRepo::get_by_name(&store, &b.name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_name.id, b.id);
    let names: Vec<String> = RepositoryRepo::list(&store)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name.to_string())
        .collect();
    assert_eq!(names, ["a-repo", "b-repo"]);
    assert!(
        RepositoryRepo::get(&store, RepositoryId::generate())
            .await
            .unwrap()
            .is_none()
    );
    let unknown = RepoName::parse("unknown").unwrap();
    assert!(
        RepositoryRepo::get_by_name(&store, &unknown)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn repository_create_conflicts_on_a_duplicate_name_or_id() {
    let store = MemStore::new();
    let original = hosted("dup");
    RepositoryRepo::create(&store, &original).await.unwrap();
    let same_name = hosted("dup");
    assert!(matches!(
        RepositoryRepo::create(&store, &same_name).await,
        Err(StoreError::Conflict(_))
    ));
    let mut same_id = hosted("other");
    same_id.id = original.id;
    assert!(matches!(
        RepositoryRepo::create(&store, &same_id).await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(RepositoryRepo::list(&store).await.unwrap().len(), 1);
}

#[tokio::test]
async fn repository_update_replaces_mutable_fields_and_keeps_created_at() {
    let store = MemStore::new();
    let mut repo = hosted("upd");
    RepositoryRepo::create(&store, &repo).await.unwrap();
    let created = repo.created_at;
    repo.online = false;
    repo.write_policy = WritePolicy::AllowOnce;
    repo.quota_bytes = Some(10);
    repo.created_at = OffsetDateTime::UNIX_EPOCH;
    RepositoryRepo::update(&store, &repo).await.unwrap();
    let stored = RepositoryRepo::get(&store, repo.id).await.unwrap().unwrap();
    assert!(!stored.online);
    assert_eq!(stored.write_policy, WritePolicy::AllowOnce);
    assert_eq!(stored.quota_bytes, Some(10));
    assert_eq!(stored.created_at, created);
}

#[tokio::test]
async fn repository_update_errors() {
    let store = MemStore::new();
    let absent = hosted("absent");
    assert!(matches!(
        RepositoryRepo::update(&store, &absent).await,
        Err(StoreError::NotFound(_))
    ));
    let first = hosted("first");
    let mut second = hosted("second");
    RepositoryRepo::create(&store, &first).await.unwrap();
    RepositoryRepo::create(&store, &second).await.unwrap();
    second.name = first.name.clone();
    assert!(matches!(
        RepositoryRepo::update(&store, &second).await,
        Err(StoreError::Conflict(_))
    ));
    let mut retyped = first.clone();
    retyped.format = RepoFormat::Npm;
    assert!(matches!(
        RepositoryRepo::update(&store, &retyped).await,
        Err(StoreError::Conflict(_))
    ));
    let mut retyped = first.clone();
    retyped.repo_type = RepoType::Group;
    assert!(matches!(
        RepositoryRepo::update(&store, &retyped).await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn repository_delete_cascades_to_components_and_assets_but_leaves_blobs() {
    let store = MemStore::new();
    let repo = hosted("doomed");
    let keep = hosted("keeper");
    RepositoryRepo::create(&store, &repo).await.unwrap();
    RepositoryRepo::create(&store, &keep).await.unwrap();
    let component = store
        .find_or_create(&coordinate(repo.id, "lib", "1"))
        .await
        .unwrap();
    let mut asset = new_asset(repo.id, "a/b.txt", b"hello");
    asset.component_id = Some(component.id);
    let blob_id = asset.blob.id;
    store.publish(&asset).await.unwrap();
    store
        .publish(&new_asset(keep.id, "kept.txt", b"kept"))
        .await
        .unwrap();

    RepositoryRepo::delete(&store, repo.id).await.unwrap();

    assert!(
        ComponentRepo::get(&store, component.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_by_path(repo.id, "a/b.txt")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_by_path(keep.id, "kept.txt")
            .await
            .unwrap()
            .is_some()
    );
    assert!(BlobRepo::get(&store, blob_id).await.unwrap().is_some());
    let far_future = OffsetDateTime::now_utc() + time::Duration::days(1);
    let unreferenced = store.list_unreferenced(far_future, 10).await.unwrap();
    assert_eq!(unreferenced.len(), 1);
    assert_eq!(unreferenced[0].id, blob_id);
    assert!(matches!(
        RepositoryRepo::delete(&store, repo.id).await,
        Err(StoreError::NotFound(_))
    ));
}

// blob stores

#[tokio::test]
async fn blob_store_crud_and_conflicts() {
    let store = MemStore::new();
    let def = blob_store_def();
    BlobStoreRepo::create(&store, &def).await.unwrap();
    assert!(matches!(
        BlobStoreRepo::create(&store, &def).await,
        Err(StoreError::Conflict(_))
    ));
    let mut same_name = blob_store_def();
    same_name.id = BlobStoreId::generate();
    assert!(matches!(
        BlobStoreRepo::create(&store, &same_name).await,
        Err(StoreError::Conflict(_))
    ));
    let mut second = blob_store_def();
    second.id = BlobStoreId::generate();
    second.name = "archive".to_owned();
    second.kind = BlobStoreKind::S3;
    BlobStoreRepo::create(&store, &second).await.unwrap();
    assert_eq!(
        BlobStoreRepo::get(&store, def.id)
            .await
            .unwrap()
            .unwrap()
            .name,
        "default"
    );
    assert_eq!(
        BlobStoreRepo::get_by_name(&store, "archive")
            .await
            .unwrap()
            .unwrap()
            .id,
        second.id
    );
    assert!(
        BlobStoreRepo::get_by_name(&store, "nope")
            .await
            .unwrap()
            .is_none()
    );
    let names: Vec<String> = BlobStoreRepo::list(&store)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["archive", "default"]);
}

#[tokio::test]
async fn blob_store_delete_conflicts_while_a_repository_uses_it() {
    let store = MemStore::new().with_default_blob_store();
    let repo = hosted("uses-store");
    RepositoryRepo::create(&store, &repo).await.unwrap();
    assert!(matches!(
        BlobStoreRepo::delete(&store, BLOB_STORE_ID).await,
        Err(StoreError::Conflict(_))
    ));
    RepositoryRepo::delete(&store, repo.id).await.unwrap();
    BlobStoreRepo::delete(&store, BLOB_STORE_ID).await.unwrap();
    assert!(matches!(
        BlobStoreRepo::delete(&store, BLOB_STORE_ID).await,
        Err(StoreError::NotFound(_))
    ));
}

// blobs

#[tokio::test]
async fn used_bytes_count_distinct_blobs_per_repository_and_all_blobs_per_store() {
    let store = MemStore::new();
    let repo = hosted("sized");
    let other = hosted("other");
    RepositoryRepo::create(&store, &repo).await.unwrap();
    RepositoryRepo::create(&store, &other).await.unwrap();
    store
        .publish(&new_asset(repo.id, "one", &[0; 100]))
        .await
        .unwrap();
    store
        .publish(&new_asset(repo.id, "two", &[0; 50]))
        .await
        .unwrap();
    store
        .publish(&new_asset(other.id, "three", &[0; 7]))
        .await
        .unwrap();
    store
        .insert_blob(blob_record("orphan", &[0; 1000]))
        .unwrap();
    assert_eq!(store.used_bytes_by_repository(repo.id).await.unwrap(), 150);
    assert_eq!(store.used_bytes_by_repository(other.id).await.unwrap(), 7);
    assert_eq!(
        store
            .used_bytes_by_repository(RepositoryId::generate())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store.used_bytes_by_store(BLOB_STORE_ID).await.unwrap(),
        1157
    );
    assert_eq!(
        store
            .used_bytes_by_store(BlobStoreId::generate())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn list_unreferenced_filters_by_age_orders_oldest_first_and_honours_limit() {
    let store = MemStore::new();
    let repo = hosted("gc");
    RepositoryRepo::create(&store, &repo).await.unwrap();
    let epoch = OffsetDateTime::UNIX_EPOCH;
    let ages = [3_i64, 1, 2];
    let mut ids = Vec::new();
    for (i, day) in ages.iter().enumerate() {
        let record = blob_record_at(&format!("k{i}"), b"x", epoch + time::Duration::days(*day));
        ids.push(record.id);
        store.insert_blob(record).unwrap();
    }
    let referenced = new_asset(repo.id, "live", b"live");
    store.publish(&referenced).await.unwrap();

    let cutoff = epoch + time::Duration::days(3);
    let listed = store.list_unreferenced(cutoff, 10).await.unwrap();
    assert_eq!(
        listed.iter().map(|b| b.id).collect::<Vec<_>>(),
        [ids[1], ids[2]],
        "cutoff is exclusive, oldest first, referenced blobs excluded"
    );
    let limited = store
        .list_unreferenced(epoch + time::Duration::days(10), 2)
        .await
        .unwrap();
    assert_eq!(
        limited.iter().map(|b| b.id).collect::<Vec<_>>(),
        [ids[1], ids[2]]
    );
}

#[tokio::test]
async fn blob_delete_removes_unreferenced_rows_only() {
    let store = MemStore::new();
    let repo = hosted("blobs");
    RepositoryRepo::create(&store, &repo).await.unwrap();
    let asset = new_asset(repo.id, "live", b"live");
    store.publish(&asset).await.unwrap();
    let orphan = blob_record("orphan", b"o");
    store.insert_blob(orphan.clone()).unwrap();
    assert!(matches!(
        store.insert_blob(orphan.clone()),
        Err(StoreError::Conflict(_))
    ));

    assert!(matches!(
        BlobRepo::delete(&store, asset.blob.id).await,
        Err(StoreError::Conflict(_))
    ));
    BlobRepo::delete(&store, orphan.id).await.unwrap();
    assert!(BlobRepo::get(&store, orphan.id).await.unwrap().is_none());
    assert!(matches!(
        BlobRepo::delete(&store, BlobId::generate()).await,
        Err(StoreError::NotFound(_))
    ));
}

// components

#[tokio::test]
async fn find_or_create_is_idempotent_per_coordinate() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let first = store
        .find_or_create(&coordinate(repo, "lib", "1.0"))
        .await
        .unwrap();
    let again = store
        .find_or_create(&coordinate(repo, "lib", "1.0"))
        .await
        .unwrap();
    assert_eq!(first.id, again.id);
    let other_version = store
        .find_or_create(&coordinate(repo, "lib", "2.0"))
        .await
        .unwrap();
    let other_repo = store
        .find_or_create(&coordinate(RepositoryId::generate(), "lib", "1.0"))
        .await
        .unwrap();
    assert_ne!(first.id, other_version.id);
    assert_ne!(first.id, other_repo.id);
    assert_eq!(
        store
            .find(&coordinate(repo, "lib", "2.0"))
            .await
            .unwrap()
            .unwrap()
            .id,
        other_version.id
    );
    assert!(
        store
            .find(&coordinate(repo, "lib", "3.0"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        ComponentRepo::get(&store, first.id)
            .await
            .unwrap()
            .unwrap()
            .name,
        "lib"
    );
    assert!(
        ComponentRepo::get(&store, ComponentId::generate())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_find_or_create_returns_one_row() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .find_or_create(&coordinate(repo, "race", "1"))
                .await
                .unwrap()
                .id
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap());
    }
    ids.dedup();
    assert_eq!(ids.len(), 1);
    let all = list_components(&store, &ComponentFilter::default(), &page(100, None))
        .await
        .unwrap();
    assert_eq!(all.items.len(), 1);
}

#[tokio::test]
async fn components_are_stamped_by_the_injected_clock() {
    let clock = FixedClock::default();
    let store = MemStore::new().with_clock(Arc::new(clock.clone()));
    let created = store
        .find_or_create(&coordinate(RepositoryId::generate(), "c", "1"))
        .await
        .unwrap();
    assert_eq!(created.created_at, clock.now());
    assert_eq!(created.updated_at, clock.now());
}

#[tokio::test]
async fn component_list_filters_by_every_field() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let other_repo = RepositoryId::generate();
    store
        .find_or_create(&coordinate(repo, "alpha", "1.0"))
        .await
        .unwrap();
    store
        .find_or_create(&coordinate(repo, "alpha", "2.0"))
        .await
        .unwrap();
    store
        .find_or_create(&coordinate(repo, "beta", "1.0"))
        .await
        .unwrap();
    store
        .find_or_create(&coordinate(other_repo, "alpha", "1.0"))
        .await
        .unwrap();
    let mut bare = coordinate(repo, "bare", "1");
    bare.group_id = None;
    bare.version = None;
    store.find_or_create(&bare).await.unwrap();

    let count = |filter: ComponentFilter| {
        let store = store.clone();
        async move {
            list_components(&store, &filter, &page(100, None))
                .await
                .unwrap()
                .items
                .len()
        }
    };
    assert_eq!(count(ComponentFilter::default()).await, 5);
    assert_eq!(
        count(ComponentFilter {
            repository_id: Some(repo),
            ..Default::default()
        })
        .await,
        4
    );
    assert_eq!(
        count(ComponentFilter {
            name: Some("alpha".into()),
            ..Default::default()
        })
        .await,
        3
    );
    assert_eq!(
        count(ComponentFilter {
            version: Some("1.0".into()),
            ..Default::default()
        })
        .await,
        3
    );
    assert_eq!(
        count(ComponentFilter {
            group_id: Some("org.example".into()),
            ..Default::default()
        })
        .await,
        4
    );
    assert_eq!(
        count(ComponentFilter {
            query: Some("ALPHA 2".into()),
            ..Default::default()
        })
        .await,
        1
    );
    assert_eq!(
        count(ComponentFilter {
            query: Some("org.example".into()),
            ..Default::default()
        })
        .await,
        4
    );
    assert_eq!(
        count(ComponentFilter {
            query: Some("   ".into()),
            ..Default::default()
        })
        .await,
        5
    );
    assert_eq!(
        count(ComponentFilter {
            query: Some("missing".into()),
            ..Default::default()
        })
        .await,
        0
    );
}

#[tokio::test]
async fn component_list_paginates_without_gaps_or_repeats() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    for i in 0..25 {
        store
            .find_or_create(&coordinate(repo, &format!("lib{i:02}"), "1"))
            .await
            .unwrap();
    }
    let filter = ComponentFilter {
        repository_id: Some(repo),
        ..Default::default()
    };
    let mut seen = Vec::new();
    let mut sizes = Vec::new();
    let mut after = None;
    loop {
        let result = list_components(&store, &filter, &page(10, after))
            .await
            .unwrap();
        sizes.push(result.items.len());
        seen.extend(result.items.into_iter().map(|c| c.id));
        match result.next {
            Some(token) => after = Some(token),
            None => break,
        }
    }
    assert_eq!(sizes, [10, 10, 5]);
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 25);
    let bad = list_components(&store, &filter, &page(10, Some("not-a-token".into()))).await;
    assert!(matches!(bad, Err(StoreError::NotFound(_))));
}

#[tokio::test]
async fn component_delete_removes_its_assets_only() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let component = store
        .find_or_create(&coordinate(repo, "lib", "1"))
        .await
        .unwrap();
    let mut owned = new_asset(repo, "lib-1.jar", b"jar");
    owned.component_id = Some(component.id);
    store.publish(&owned).await.unwrap();
    store
        .publish(&new_asset(repo, "loose.txt", b"loose"))
        .await
        .unwrap();

    ComponentRepo::delete(&store, component.id).await.unwrap();

    assert!(
        store
            .get_by_path(repo, "lib-1.jar")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_by_path(repo, "loose.txt")
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        ComponentRepo::delete(&store, component.id).await,
        Err(StoreError::NotFound(_))
    ));
}

// assets

#[tokio::test]
async fn publish_of_a_new_path_creates_the_asset_and_the_blob_row() {
    let clock = FixedClock::default();
    let store = MemStore::new().with_clock(Arc::new(clock.clone()));
    let repo = RepositoryId::generate();
    let new = new_asset(repo, "dir/file.bin", b"payload");
    let outcome = store.publish(&new).await.unwrap();
    assert!(outcome.replaced.is_none());
    let asset = outcome.asset;
    assert_eq!(asset.path, "dir/file.bin");
    assert_eq!(asset.blob_id, new.blob.id);
    assert_eq!(asset.size_bytes, 7);
    assert_eq!(asset.checksums, new.blob.checksums);
    assert_eq!(asset.content_type, "application/octet-stream");
    assert!(asset.last_downloaded_at.is_none());
    let by_path = store
        .get_by_path(repo, "dir/file.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_path.id, asset.id);
    assert_eq!(
        AssetRepo::get(&store, asset.id)
            .await
            .unwrap()
            .unwrap()
            .path,
        "dir/file.bin"
    );
    assert_eq!(
        BlobRepo::get(&store, new.blob.id)
            .await
            .unwrap()
            .unwrap()
            .key,
        new.blob.key
    );
    assert!(
        store
            .get_by_path(RepositoryId::generate(), "dir/file.bin")
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.get_by_path(repo, "dir").await.unwrap().is_none());
}

#[tokio::test]
async fn publish_over_an_existing_path_swaps_the_blob_and_returns_the_old_one() {
    let clock = FixedClock::default();
    let store = MemStore::new().with_clock(Arc::new(clock.clone()));
    let repo = RepositoryId::generate();
    let first = new_asset(repo, "same/path", b"old");
    let created = store.publish(&first).await.unwrap().asset;
    clock.advance(Duration::from_secs(60));
    let second = new_asset(repo, "same/path", b"newer content");
    let outcome = store.publish(&second).await.unwrap();

    assert_eq!(outcome.replaced.as_ref().map(|b| b.id), Some(first.blob.id));
    assert_eq!(
        outcome.asset.id, created.id,
        "the asset row is switched, not recreated"
    );
    assert_eq!(outcome.asset.blob_id, second.blob.id);
    assert_eq!(outcome.asset.size_bytes, 13);
    assert_eq!(outcome.asset.created_at, created.created_at);
    assert!(outcome.asset.updated_at > created.updated_at);
    let current = store.get_by_path(repo, "same/path").await.unwrap().unwrap();
    assert_eq!(current.blob_id, second.blob.id);
    assert_eq!(store.used_bytes_by_repository(repo).await.unwrap(), 13);
    let garbage = store
        .list_unreferenced(OffsetDateTime::now_utc() + time::Duration::days(1), 10)
        .await
        .unwrap();
    assert_eq!(
        garbage.iter().map(|b| b.id).collect::<Vec<_>>(),
        [first.blob.id]
    );
}

#[tokio::test]
async fn must_not_change_anything_when_publish_hits_a_duplicate_blob_id() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let first = new_asset(repo, "p", b"original");
    store.publish(&first).await.unwrap();
    let mut clash = new_asset(repo, "p", b"replacement");
    clash.blob.id = first.blob.id;
    assert!(matches!(
        store.publish(&clash).await,
        Err(StoreError::Conflict(_))
    ));
    let mut elsewhere = new_asset(repo, "q", b"other");
    elsewhere.blob.id = first.blob.id;
    assert!(matches!(
        store.publish(&elsewhere).await,
        Err(StoreError::Conflict(_))
    ));
    let current = store.get_by_path(repo, "p").await.unwrap().unwrap();
    assert_eq!(current.blob_id, first.blob.id);
    assert_eq!(current.size_bytes, 8);
    assert!(store.get_by_path(repo, "q").await.unwrap().is_none());
}

#[tokio::test]
async fn delete_by_path_returns_the_blob_and_forgets_the_asset() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let new = new_asset(repo, "to/delete", b"bye");
    store.publish(&new).await.unwrap();
    let freed = store
        .delete_by_path(repo, "to/delete")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(freed.id, new.blob.id);
    assert!(
        store
            .get_by_path(repo, "to/delete")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .delete_by_path(repo, "to/delete")
            .await
            .unwrap()
            .is_none()
    );
    assert!(BlobRepo::get(&store, new.blob.id).await.unwrap().is_some());
}

#[tokio::test]
async fn asset_list_filters_by_prefix_and_repository_and_paginates() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let other = RepositoryId::generate();
    for i in 0..12 {
        store
            .publish(&new_asset(repo, &format!("docs/page-{i:02}.md"), b"d"))
            .await
            .unwrap();
    }
    store
        .publish(&new_asset(repo, "src/main.rs", b"m"))
        .await
        .unwrap();
    store
        .publish(&new_asset(other, "docs/foreign.md", b"f"))
        .await
        .unwrap();

    let first = AssetRepo::list(&store, repo, "docs/", &page(5, None))
        .await
        .unwrap();
    assert_eq!(first.items.len(), 5);
    assert_eq!(first.items[0].path, "docs/page-00.md");
    let second = AssetRepo::list(&store, repo, "docs/", &page(5, first.next))
        .await
        .unwrap();
    let third = AssetRepo::list(&store, repo, "docs/", &page(5, second.next))
        .await
        .unwrap();
    assert_eq!(
        (second.items.len(), third.items.len(), third.next.is_none()),
        (5, 2, true)
    );
    assert_eq!(third.items[1].path, "docs/page-11.md");

    let everything = AssetRepo::list(&store, repo, "", &page(100, None))
        .await
        .unwrap();
    assert_eq!(everything.items.len(), 13);
    let none = AssetRepo::list(&store, repo, "nothing/", &page(100, None))
        .await
        .unwrap();
    assert!(none.items.is_empty() && none.next.is_none());
}

#[tokio::test]
async fn list_by_component_returns_that_components_assets_sorted_by_path() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let component = store
        .find_or_create(&coordinate(repo, "lib", "1"))
        .await
        .unwrap();
    for path in ["lib-1.pom", "lib-1.jar"] {
        let mut asset = new_asset(repo, path, b"x");
        asset.component_id = Some(component.id);
        store.publish(&asset).await.unwrap();
    }
    store
        .publish(&new_asset(repo, "unrelated", b"x"))
        .await
        .unwrap();
    let paths: Vec<String> = store
        .list_by_component(component.id)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.path)
        .collect();
    assert_eq!(paths, ["lib-1.jar", "lib-1.pom"]);
}

#[tokio::test]
async fn touch_downloaded_only_moves_forward_and_ignores_unknown_assets() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let asset = store
        .publish(&new_asset(repo, "dl", b"x"))
        .await
        .unwrap()
        .asset;
    let t0 = OffsetDateTime::UNIX_EPOCH;
    let t1 = t0 + time::Duration::hours(1);
    let ghost = rustryx_core::ids::AssetId::generate();
    store
        .touch_downloaded(&[(asset.id, t1), (ghost, t1)])
        .await
        .unwrap();
    store.touch_downloaded(&[(asset.id, t0)]).await.unwrap();
    let stored = AssetRepo::get(&store, asset.id).await.unwrap().unwrap();
    assert_eq!(stored.last_downloaded_at, Some(t1));
    store.touch_downloaded(&[]).await.unwrap();
}

#[tokio::test]
async fn republishing_keeps_the_download_stamp() {
    let store = MemStore::new();
    let repo = RepositoryId::generate();
    let asset = store
        .publish(&new_asset(repo, "dl", b"x"))
        .await
        .unwrap()
        .asset;
    let stamp = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
    store.touch_downloaded(&[(asset.id, stamp)]).await.unwrap();
    let republished = store
        .publish(&new_asset(repo, "dl", b"y"))
        .await
        .unwrap()
        .asset;
    assert_eq!(republished.last_downloaded_at, Some(stamp));
}

#[tokio::test]
async fn unavailable_store_fails_content_ports_too() {
    let store = MemStore::new();
    store.set_unavailable(true);
    let repo = RepositoryId::generate();
    assert!(matches!(
        store.get_by_path(repo, "x").await,
        Err(StoreError::Unavailable(_))
    ));
    assert!(matches!(
        store.find_or_create(&coordinate(repo, "x", "1")).await,
        Err(StoreError::Unavailable(_))
    ));
    assert!(matches!(
        RepositoryRepo::list(&store).await,
        Err(StoreError::Unavailable(_))
    ));
    assert!(matches!(
        store.insert_blob(blob_record("k", b"x")),
        Err(StoreError::Unavailable(_))
    ));
    store.set_unavailable(false);
    assert!(RepositoryRepo::list(&store).await.unwrap().is_empty());
}
