//! Cleanup policies are unique, versioned, and protected while a repository lists them.

use rustryx_core::StoreError;
use rustryx_core::ids::CleanupPolicyId;
use rustryx_core::lifecycle::{CleanupCriteria, CleanupPolicy};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{CleanupPolicyRepo, RepositoryRepo};
use rustryx_db::repos::cleanup::PgCleanupPolicyRepo;
use time::OffsetDateTime;

use crate::fixtures::seed_repository;
use crate::support::TestDb;

fn policy(name: &str) -> CleanupPolicy {
    let mut criteria = CleanupCriteria::default();
    criteria.max_age_days = Some(30);
    let mut policy = CleanupPolicy::new(
        CleanupPolicyId::generate(),
        name,
        criteria,
        OffsetDateTime::now_utc(),
    );
    policy.format = Some(RepoFormat::Raw);
    policy
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_policies_are_unique_versioned_and_protected_while_attached() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let policies = PgCleanupPolicyRepo::new(store.pool().clone());
    let first = policy("weekly");
    policies.create(&first).await.unwrap();
    assert!(matches!(
        policies.create(&first).await,
        Err(StoreError::Conflict(ref message)) if message.contains("already")
    ));
    assert!(matches!(
        policies.create(&policy("weekly")).await,
        Err(StoreError::Conflict(ref message)) if message.contains("already")
    ));
    let empty = CleanupPolicy::new(
        CleanupPolicyId::generate(),
        "empty",
        CleanupCriteria::default(),
        OffsetDateTime::now_utc(),
    );
    assert!(matches!(
        policies.create(&empty).await,
        Err(StoreError::Conflict(_))
    ));
    policies.create(&policy("alpha")).await.unwrap();
    let names: Vec<String> = policies
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(names, ["alpha", "weekly"]);

    let stored = policies.get(first.id).await.unwrap().unwrap();
    assert_eq!(stored.format, Some(RepoFormat::Raw));
    assert_eq!(stored.criteria.max_age_days, Some(30));
    let mut edited = stored.clone();
    edited.description = "kept".to_owned();
    policies.update(&edited, 1).await.unwrap();
    let after = policies.get(first.id).await.unwrap().unwrap();
    assert_eq!(after.version, 2);
    assert_eq!(after.description, "kept");
    assert_eq!(after.created_at, stored.created_at);
    assert!(matches!(
        policies.update(&edited, 1).await,
        Err(StoreError::Conflict(ref message)) if message.contains("was changed by someone else")
    ));
    let mut clash = after.clone();
    clash.name = "alpha".to_owned();
    assert!(matches!(
        policies.update(&clash, 2).await,
        Err(StoreError::Conflict(ref message)) if message.contains("already")
    ));
    let mut bad = after.clone();
    bad.criteria = CleanupCriteria::default();
    assert!(matches!(
        policies.update(&bad, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = after.clone();
    ghost.id = CleanupPolicyId::generate();
    assert!(matches!(
        policies.update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));

    let (_blob_store, mut repo) = seed_repository(&store).await;
    repo.cleanup_policy_ids = vec![first.id];
    store.repositories.update(&repo).await.unwrap();
    assert_eq!(
        policies.repositories_using(first.id).await.unwrap(),
        [repo.id]
    );
    assert!(matches!(
        policies.delete(first.id).await,
        Err(StoreError::Conflict(_))
    ));
    repo.cleanup_policy_ids.clear();
    store.repositories.update(&repo).await.unwrap();
    policies.delete(first.id).await.unwrap();
    assert!(matches!(
        policies.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
}
