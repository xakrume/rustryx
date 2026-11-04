//! A deletion batch skips an immutable component and a hold, records the decision, and
//! lets an admin override lift only the immutability rule.

use rustryx_core::ids::{ComponentId, ImmutabilityRuleId, LifecycleHoldId, LifecycleRunId};
use rustryx_core::lifecycle::{
    HoldKind, HoldTarget, ImmutabilityRule, ItemDecision, ItemKind, LifecycleKind, LifecycleRun,
    NameSelector, PolicyScope,
};
use rustryx_core::model::WriteOptions;
use rustryx_core::ports::{
    AssetRepo, ComponentRepo, DeleteResult, ImmutabilityRepo, LifecycleHoldRepo, LifecycleRepo,
    LifecycleRunRepo, PageRequest,
};
use rustryx_db::repos::lifecycle_hold::PgLifecycleHoldRepo;
use rustryx_db::repos::{PgImmutabilityRepo, PgLifecycleRepo, PgLifecycleRunRepo};
use time::OffsetDateTime;

use crate::fixtures::{blob, new_asset, new_component, seed_repository};
use crate::support::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn apply_deletions_skips_what_is_protected_and_records_what_it_did() {
    let db = TestDb::new().await;
    let store = db.store().await;
    let pool = store.pool().clone();
    let (blob_store, repo) = seed_repository(&store).await;
    let plain = store
        .components
        .find_or_create(&new_component(repo.id, None, "plain", Some("1")))
        .await
        .unwrap();
    let frozen = store
        .components
        .find_or_create(&new_component(repo.id, None, "frozen", Some("1.0")))
        .await
        .unwrap();
    let held = store
        .components
        .find_or_create(&new_component(repo.id, None, "held", Some("9")))
        .await
        .unwrap();

    let version_rule = ImmutabilityRule::new(
        ImmutabilityRuleId::generate(),
        PolicyScope::Repository(repo.id),
        "releases",
        ItemKind::Version,
        NameSelector::matching(vec!["1.*".to_owned()]).unwrap(),
        OffsetDateTime::now_utc(),
    );
    PgImmutabilityRepo::new(pool.clone())
        .create(&version_rule)
        .await
        .unwrap();
    let on_held = rustryx_core::lifecycle::LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Legal,
        HoldTarget::Component(held.id),
        "litigation",
        OffsetDateTime::now_utc(),
    );
    PgLifecycleHoldRepo::new(pool.clone())
        .create(&on_held)
        .await
        .unwrap();

    let mut asset = new_asset(repo.id, "plain/file", blob(blob_store.id, 4));
    asset.component_id = Some(plain.id);
    store
        .assets
        .publish_with(&asset, &WriteOptions::new())
        .await
        .unwrap();

    let run = LifecycleRun::new(LifecycleRunId::generate(), LifecycleKind::Cleanup, false, 1);
    let runs = PgLifecycleRunRepo::new(pool.clone());
    runs.create(&run).await.unwrap();
    let lifecycle = PgLifecycleRepo::new(pool);
    let batch = [plain.id, frozen.id, held.id, ComponentId::generate()];
    let results = lifecycle
        .apply_deletions(run.id, &batch, &WriteOptions::new())
        .await
        .unwrap();
    assert_eq!(
        results,
        [
            DeleteResult::Deleted { bytes: 4 },
            DeleteResult::Skipped(ItemDecision::ProtectedImmutable),
            DeleteResult::Skipped(ItemDecision::ProtectedHold),
            DeleteResult::Failed,
        ]
    );
    assert!(store.components.get(plain.id).await.unwrap().is_none());
    assert!(
        store
            .assets
            .get_by_path(repo.id, "plain/file")
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.components.get(frozen.id).await.unwrap().is_some());
    assert!(store.components.get(held.id).await.unwrap().is_some());

    let items = runs
        .list_items(
            run.id,
            &PageRequest {
                limit: 10,
                after: None,
            },
        )
        .await
        .unwrap();
    let decisions: Vec<ItemDecision> = items.items.iter().map(|item| item.decision).collect();
    assert_eq!(
        decisions,
        [
            ItemDecision::Delete,
            ItemDecision::ProtectedImmutable,
            ItemDecision::ProtectedHold
        ]
    );
    assert_eq!(items.items[0].reason, "applied");
    assert_eq!(items.items[0].size_bytes, 4);
    assert_eq!(items.items[1].reason, version_rule.id.to_string());
    assert_eq!(items.items[1].size_bytes, 0);
    assert_eq!(items.items[2].reason, on_held.id.to_string());
    assert_eq!(items.items.len(), 3, "a missing component writes no item");

    let overridden = lifecycle
        .apply_deletions(
            run.id,
            &[frozen.id, held.id],
            &WriteOptions::new().with_override_immutable(true),
        )
        .await
        .unwrap();
    assert_eq!(
        overridden,
        [
            DeleteResult::Deleted { bytes: 0 },
            DeleteResult::Skipped(ItemDecision::ProtectedHold)
        ]
    );
    assert!(store.components.get(frozen.id).await.unwrap().is_none());
    assert!(store.components.get(held.id).await.unwrap().is_some());
}
