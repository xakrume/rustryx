//! Blob store groups: membership order, removal guards, and conversion.

use rustryx_core::StoreError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{BlobStoreDef, BlobStoreKind};
use rustryx_core::ports::{BlobRepo, BlobStoreMemberRepo, BlobStoreRepo, RepositoryRepo};
use rustryx_core::storage::MemberState;
use rustryx_db::repos::PgBlobStoreMemberRepo;

use crate::fixtures::{blob, blob_store, seed_repository, unique};
use crate::support::TestDb;

fn group_store(name: &str) -> BlobStoreDef {
    let mut group = blob_store(name);
    group.kind = BlobStoreKind::Group;
    group.config = serde_json::json!({});
    group
}

async fn two_stores(store: &rustryx_db::PgStore) -> (BlobStoreDef, BlobStoreDef) {
    let (first, second) = (
        blob_store(&unique("member-a")),
        blob_store(&unique("member-b")),
    );
    store.blob_stores.create(&first).await.unwrap();
    store.blob_stores.create(&second).await.unwrap();
    (first, second)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_holds_plain_stores_in_position_order_and_each_store_joins_one_group() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let members = PgBlobStoreMemberRepo::new(store.pool().clone());
    let (first, second) = two_stores(&store).await;
    let group = group_store(&unique("pool"));
    let other_group = group_store(&unique("other-pool"));
    store.blob_stores.create(&group).await.unwrap();
    store.blob_stores.create(&other_group).await.unwrap();
    members.add_member(group.id, second.id, 2).await.unwrap();
    members.add_member(group.id, first.id, 1).await.unwrap();
    let listed = members.members(group.id).await.unwrap();
    assert_eq!(
        listed.iter().map(|m| m.member_id).collect::<Vec<_>>(),
        [first.id, second.id]
    );
    assert!(listed.iter().all(|m| m.state == MemberState::Active));
    assert_eq!(members.group_of(first.id).await.unwrap(), Some(group.id));
    assert_eq!(members.group_of(group.id).await.unwrap(), None);
    assert_eq!(
        members.group_of(BlobStoreId::generate()).await.unwrap(),
        None
    );

    assert!(matches!(
        members.add_member(other_group.id, first.id, 1).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        members.add_member(group.id, other_group.id, 3).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        members.add_member(group.id, group.id, 3).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        members.add_member(first.id, second.id, 3).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        members
            .add_member(group.id, BlobStoreId::generate(), 3)
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        members
            .add_member(BlobStoreId::generate(), first.id, 3)
            .await,
        Err(StoreError::Conflict(_))
    ));

    members
        .set_state(group.id, first.id, MemberState::Draining)
        .await
        .unwrap();
    assert_eq!(
        members.members(group.id).await.unwrap()[0].state,
        MemberState::Draining
    );
    assert!(matches!(
        members
            .set_state(group.id, BlobStoreId::generate(), MemberState::Active)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.blob_stores.delete(first.id).await,
        Err(StoreError::Conflict(_))
    ));
    store.blob_stores.delete(group.id).await.unwrap();
    assert_eq!(members.group_of(first.id).await.unwrap(), None);
    store.blob_stores.delete(first.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_remove_a_member_that_still_holds_blobs_or_the_last_one_of_a_used_group() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let members = PgBlobStoreMemberRepo::new(store.pool().clone());
    let (first, second) = two_stores(&store).await;
    let group = group_store(&unique("busy-pool"));
    store.blob_stores.create(&group).await.unwrap();
    members.add_member(group.id, first.id, 0).await.unwrap();
    members.add_member(group.id, second.id, 1).await.unwrap();
    let record = blob(first.id, 1);
    store.blobs.insert(&record).await.unwrap();
    assert!(matches!(
        members.remove_member(group.id, first.id).await,
        Err(StoreError::Conflict(_))
    ));

    let repo = crate::fixtures::hosted_repository(&unique("pooled"), group.id);
    store.repositories.create(&repo).await.unwrap();
    members.remove_member(group.id, second.id).await.unwrap();
    let last = members
        .remove_member(group.id, second.id)
        .await
        .unwrap_err();
    assert!(matches!(last, StoreError::NotFound(_)), "already gone");
    store.blobs.delete(record.id).await.unwrap();
    let emptied = members.remove_member(group.id, first.id).await.unwrap_err();
    assert!(matches!(emptied, StoreError::Conflict(_)));
    store.repositories.delete(repo.id).await.unwrap();
    members.remove_member(group.id, first.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn converting_a_store_into_a_group_repoints_its_repositories_in_one_step() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let members = PgBlobStoreMemberRepo::new(store.pool().clone());
    let (existing, repo) = seed_repository(&store).await;
    let record = blob(existing.id, 4);
    store.blobs.insert(&record).await.unwrap();
    let group = group_store(&unique("converted"));
    members.convert_to_group(existing.id, &group).await.unwrap();
    assert_eq!(
        store
            .repositories
            .get(repo.id)
            .await
            .unwrap()
            .unwrap()
            .blob_store_id,
        group.id
    );
    let listed = members.members(group.id).await.unwrap();
    assert_eq!(
        (listed.len(), listed[0].member_id, listed[0].position),
        (1, existing.id, 0)
    );
    assert_eq!(
        store
            .blobs
            .get(record.id)
            .await
            .unwrap()
            .unwrap()
            .blob_store_id,
        existing.id
    );

    assert!(matches!(
        members
            .convert_to_group(group.id, &group_store(&unique("again")))
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        members
            .convert_to_group(existing.id, &group_store(&unique("second")))
            .await,
        Err(StoreError::Conflict(_))
    ));
    let plain = blob_store(&unique("plain"));
    store.blob_stores.create(&plain).await.unwrap();
    assert!(matches!(
        members
            .convert_to_group(plain.id, &blob_store(&unique("not-a-group")))
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        members.convert_to_group(plain.id, &group).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        members
            .convert_to_group(BlobStoreId::generate(), &group_store(&unique("missing")))
            .await,
        Err(StoreError::NotFound(_))
    ));
}
