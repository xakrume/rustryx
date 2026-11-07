//! An approved promotion links blobs into the target and deletes the source only on move.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rustryx_core::ids::{BlobId, PromotionId, UserId};
use rustryx_core::model::{NewAsset, NewComponent, RepoFormat, RepoType};
use rustryx_core::ports::{AssetRepo, BlobStoreRepo, ComponentRepo, PromotionRepo, RepositoryRepo};
use rustryx_core::promotion::{
    PromotionMode, PromotionRequest, PromotionSelector, PromotionStatus,
};
use rustryx_core::promotion_run;
use rustryx_testkit::MemStore;
use rustryx_testkit::fixtures::{blob_record, blob_store_def, repository};
use time::OffsetDateTime;

fn at() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
}

async fn world() -> (
    MemStore,
    rustryx_core::model::Repository,
    rustryx_core::model::Repository,
) {
    let store = MemStore::new();
    BlobStoreRepo::create(&store, &blob_store_def())
        .await
        .unwrap();
    let source = repository("src", RepoFormat::Raw, RepoType::Hosted);
    let target = repository("dst", RepoFormat::Raw, RepoType::Hosted);
    RepositoryRepo::create(&store, &source).await.unwrap();
    RepositoryRepo::create(&store, &target).await.unwrap();
    (store, source, target)
}

async fn seed(
    store: &MemStore,
    repo: rustryx_core::ids::RepositoryId,
) -> rustryx_core::ids::ComponentId {
    let mut coordinate = NewComponent::new(repo, RepoFormat::Raw, "pkg");
    coordinate.version = Some("1".to_owned());
    let component = ComponentRepo::find_or_create(store, &coordinate)
        .await
        .unwrap();
    let mut asset = NewAsset::new(
        repo,
        "pkg-1.bin",
        "application/octet-stream",
        blob_record(&format!("key-{}", BlobId::generate()), b"bytes"),
    );
    asset.component_id = Some(component.id);
    AssetRepo::publish(store, &asset).await.unwrap();
    component.id
}

#[tokio::test]
async fn a_copy_links_the_blob_and_keeps_the_source() {
    let (store, source, target) = world().await;
    let component_id = seed(&store, source.id).await;
    let mut request = PromotionRequest::new(
        PromotionId::generate(),
        PromotionMode::Copy,
        source.id,
        target.id,
        PromotionSelector::Components {
            ids: vec![component_id],
        },
        UserId::generate(),
        at(),
    );
    request.status = PromotionStatus::Approved;
    PromotionRepo::create(&store, &request, &[component_id], &[])
        .await
        .unwrap();
    let status = promotion_run::execute(&store, &store, &store, request.id, at())
        .await
        .unwrap();
    assert_eq!(status, PromotionStatus::Succeeded);
    let copied = AssetRepo::get_by_path(&store, target.id, "pkg-1.bin")
        .await
        .unwrap()
        .unwrap();
    let original = AssetRepo::get_by_path(&store, source.id, "pkg-1.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(copied.blob_id, original.blob_id);
    assert!(
        ComponentRepo::get(&store, component_id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_move_removes_the_source_component() {
    let (store, source, target) = world().await;
    let component_id = seed(&store, source.id).await;
    let mut request = PromotionRequest::new(
        PromotionId::generate(),
        PromotionMode::Move,
        source.id,
        target.id,
        PromotionSelector::Components {
            ids: vec![component_id],
        },
        UserId::generate(),
        at(),
    );
    request.status = PromotionStatus::Approved;
    PromotionRepo::create(&store, &request, &[component_id], &[])
        .await
        .unwrap();
    let status = promotion_run::execute(&store, &store, &store, request.id, at())
        .await
        .unwrap();
    assert_eq!(status, PromotionStatus::Succeeded);
    assert!(
        AssetRepo::get_by_path(&store, target.id, "pkg-1.bin")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        ComponentRepo::get(&store, component_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_required_check_fails_the_item_instead_of_copying() {
    let (store, source, target) = world().await;
    let component_id = seed(&store, source.id).await;
    let mut request = PromotionRequest::new(
        PromotionId::generate(),
        PromotionMode::Copy,
        source.id,
        target.id,
        PromotionSelector::Components {
            ids: vec![component_id],
        },
        UserId::generate(),
        at(),
    );
    request.status = PromotionStatus::Approved;
    request.checks = vec![rustryx_core::promotion::PromotionCheck::SbomPresent];
    PromotionRepo::create(&store, &request, &[component_id], &[])
        .await
        .unwrap();
    let status = promotion_run::execute(&store, &store, &store, request.id, at())
        .await
        .unwrap();
    assert_eq!(status, PromotionStatus::Failed);
    assert!(
        AssetRepo::get_by_path(&store, target.id, "pkg-1.bin")
            .await
            .unwrap()
            .is_none()
    );
}
