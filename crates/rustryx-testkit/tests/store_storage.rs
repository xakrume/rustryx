//! Blob store groups and members, the move ledger, verification runs and store statistics.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    clippy::assert_is_empty,
    reason = "each test is one scenario read top to bottom; filters and non exhaustive types are \
              filled field by field after `default()`, and `.is_empty()` reads better than a \
              comparison with an empty literal of an inferred type"
)]

mod common;

use common::{Ports, T0, env, hosted, page};
use futures::StreamExt;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{BlobId, BlobStoreId, VerifyRunId};
use rustryx_core::model::{BlobStoreDef, BlobStoreKind};
use rustryx_core::ports::{InvalidationBus, InvalidationEvent};
use rustryx_core::storage::{
    BlobMove, BlobStoreStats, MemberState, VerifyCounts, VerifyFinding, VerifyFindingKind,
    VerifyMode, VerifyRun,
};
use rustryx_core::tasks::TaskRunStatus;
use rustryx_testkit::fixtures::{BLOB_STORE_ID, blob_record_at};
use rustryx_testkit::{MemInvalidationBus, MemStore};

fn plain_store(name: &str) -> BlobStoreDef {
    BlobStoreDef::new(
        BlobStoreId::generate(),
        name,
        BlobStoreKind::Local,
        serde_json::json!({}),
        T0,
    )
}

fn group_store(name: &str) -> BlobStoreDef {
    let mut group = plain_store(name);
    group.kind = BlobStoreKind::Group;
    group
}

async fn two_stores(store: &MemStore) -> (BlobStoreDef, BlobStoreDef) {
    let (first, second) = (plain_store("member-a"), plain_store("member-b"));
    store.blob_stores().create(&first).await.unwrap();
    store.blob_stores().create(&second).await.unwrap();
    (first, second)
}

// blob store definitions

#[tokio::test]
async fn blob_store_updates_keep_the_kind_and_tell_the_bus() {
    let env = env();
    let bus = MemInvalidationBus::new();
    let store = env.store.clone().with_invalidation_bus(bus.clone());
    let mut events = bus.subscribe();
    let created = plain_store("editable");
    store.blob_stores().create(&created).await.unwrap();
    let mut edited = created.clone();
    edited.quota_bytes = Some(1000);
    edited.config = serde_json::json!({ "path": "/elsewhere" });
    edited.created_at = T0 + time::Duration::days(9);
    store.blob_stores().update(&edited).await.unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::Reset));
    let stored = store.blob_stores().get(created.id).await.unwrap().unwrap();
    assert_eq!(stored.quota_bytes, Some(1000));
    assert_eq!(
        stored.created_at, created.created_at,
        "the creation time is not editable"
    );

    let mut other_kind = created.clone();
    other_kind.kind = BlobStoreKind::S3;
    assert!(matches!(
        store.blob_stores().update(&other_kind).await,
        Err(StoreError::Conflict(_))
    ));
    store
        .blob_stores()
        .create(&plain_store("taken"))
        .await
        .unwrap();
    let mut renamed = created.clone();
    renamed.name = "taken".to_owned();
    assert!(matches!(
        store.blob_stores().update(&renamed).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = created.clone();
    ghost.id = BlobStoreId::generate();
    assert!(matches!(
        store.blob_stores().update(&ghost).await,
        Err(StoreError::NotFound(_))
    ));
}

// groups

#[tokio::test]
async fn a_group_holds_plain_stores_in_position_order_and_each_store_joins_one_group() {
    let env = env();
    let (first, second) = two_stores(&env.store).await;
    let group = group_store("pool");
    let other_group = group_store("other-pool");
    env.store.blob_stores().create(&group).await.unwrap();
    env.store.blob_stores().create(&other_group).await.unwrap();
    let members = env.store.store_members();
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

    // No sharing, no nesting, no joining yourself, and both rows must exist.
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
    assert!(
        matches!(
            members.add_member(first.id, second.id, 3).await,
            Err(StoreError::Conflict(_))
        ),
        "the group must be a group"
    );
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
    // A member cannot be deleted from under its group, a group takes its member rows along.
    assert!(matches!(
        env.store.blob_stores().delete(first.id).await,
        Err(StoreError::Conflict(_))
    ));
    env.store.blob_stores().delete(group.id).await.unwrap();
    assert_eq!(members.group_of(first.id).await.unwrap(), None);
    env.store.blob_stores().delete(first.id).await.unwrap();
}

#[tokio::test]
async fn must_not_remove_a_member_that_still_holds_blobs_or_the_last_one_of_a_used_group() {
    let env = env();
    let (first, second) = two_stores(&env.store).await;
    let group = group_store("busy-pool");
    env.store.blob_stores().create(&group).await.unwrap();
    let members = env.store.store_members();
    members.add_member(group.id, first.id, 0).await.unwrap();
    members.add_member(group.id, second.id, 1).await.unwrap();
    let mut blob = blob_record_at("held", b"x", T0);
    blob.blob_store_id = first.id;
    env.store.blobs().insert(&blob).await.unwrap();
    assert!(matches!(
        members.remove_member(group.id, first.id).await,
        Err(StoreError::Conflict(_))
    ));

    let mut repo = rustryx_testkit::fixtures::repository(
        "pooled",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repo.blob_store_id = group.id;
    env.store.repos().create(&repo).await.unwrap();
    members.remove_member(group.id, second.id).await.unwrap();
    let last = members
        .remove_member(group.id, second.id)
        .await
        .unwrap_err();
    assert!(matches!(last, StoreError::NotFound(_)), "already gone");
    env.store.blobs().delete(blob.id).await.unwrap();
    let emptied = members.remove_member(group.id, first.id).await.unwrap_err();
    assert!(
        matches!(emptied, StoreError::Conflict(_)),
        "a group used by repositories needs a member"
    );
    env.store.repos().delete(repo.id).await.unwrap();
    members.remove_member(group.id, first.id).await.unwrap();
}

#[tokio::test]
async fn converting_a_store_into_a_group_repoints_its_repositories_in_one_step() {
    let env = env();
    let bus = MemInvalidationBus::new();
    let store = env.store.clone().with_invalidation_bus(bus.clone());
    let mut events = bus.subscribe();
    let repo = hosted(&store, "convertible").await;
    let group = group_store("converted");
    store
        .store_members()
        .convert_to_group(BLOB_STORE_ID, &group)
        .await
        .unwrap();
    assert_eq!(events.next().await, Some(InvalidationEvent::Reset));
    assert_eq!(
        store
            .repos()
            .get(repo.id)
            .await
            .unwrap()
            .unwrap()
            .blob_store_id,
        group.id
    );
    let members = store.store_members().members(group.id).await.unwrap();
    assert_eq!(
        (members.len(), members[0].member_id, members[0].position),
        (1, BLOB_STORE_ID, 0)
    );

    assert!(matches!(
        store
            .store_members()
            .convert_to_group(group.id, &group_store("again"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        matches!(
            store
                .store_members()
                .convert_to_group(BLOB_STORE_ID, &group_store("second"))
                .await,
            Err(StoreError::Conflict(_))
        ),
        "it already belongs to a group"
    );
    let plain = plain_store("plain");
    store.blob_stores().create(&plain).await.unwrap();
    assert!(matches!(
        store
            .store_members()
            .convert_to_group(plain.id, &plain_store("not-a-group"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        matches!(
            store
                .store_members()
                .convert_to_group(plain.id, &group)
                .await,
            Err(StoreError::Conflict(_))
        ),
        "the group name or id exists"
    );
    assert!(matches!(
        store
            .store_members()
            .convert_to_group(BlobStoreId::generate(), &group_store("x"))
            .await,
        Err(StoreError::NotFound(_))
    ));
}

// move ledger

#[tokio::test]
async fn the_move_ledger_keeps_one_row_per_blob_and_source_store_until_cleaned() {
    let env = env();
    let (from, to) = two_stores(&env.store).await;
    let blob = BlobId::generate();
    let first = BlobMove::new(blob, from.id, to.id, "key-1", T0, 600, 30).unwrap();
    let second = BlobMove::new(
        blob,
        to.id,
        from.id,
        "key-2",
        T0 + time::Duration::minutes(1),
        600,
        30,
    )
    .unwrap();
    let ledger = env.store.blob_moves();
    ledger.record(&first).await.unwrap();
    ledger.record(&second).await.unwrap();
    ledger.record(&first).await.unwrap();
    assert!(
        ledger
            .due_for_cleanup(T0 + time::Duration::minutes(9), 10)
            .await
            .unwrap()
            .is_empty()
    );
    let due = ledger
        .due_for_cleanup(T0 + time::Duration::seconds(630), 10)
        .await
        .unwrap();
    assert_eq!(
        due.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(),
        ["key-1"]
    );
    let both = ledger
        .due_for_cleanup(T0 + time::Duration::minutes(12), 10)
        .await
        .unwrap();
    assert_eq!(
        both.len(),
        2,
        "two moves inside the window keep both old objects"
    );
    assert_eq!(
        ledger
            .due_for_cleanup(T0 + time::Duration::minutes(12), 1)
            .await
            .unwrap()
            .len(),
        1
    );
    ledger.mark_cleaned(blob, from.id).await.unwrap();
    ledger.mark_cleaned(blob, from.id).await.unwrap();
    ledger
        .mark_cleaned(BlobId::generate(), from.id)
        .await
        .unwrap();
    let rest = ledger
        .due_for_cleanup(T0 + time::Duration::minutes(12), 10)
        .await
        .unwrap();
    assert_eq!(
        rest.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(),
        ["key-2"]
    );
}

// verification

fn run_for(store: BlobStoreId) -> VerifyRun {
    VerifyRun::new(VerifyRunId::generate(), store, VerifyMode::Report)
}

#[tokio::test]
async fn a_verification_run_compares_the_staged_listing_with_the_rows() {
    let env = env();
    let runs = env.store.verify_runs();
    let run = run_for(BLOB_STORE_ID);
    runs.create(&run).await.unwrap();
    assert!(matches!(
        runs.create(&run).await,
        Err(StoreError::Conflict(_))
    ));
    let mut invalid = run_for(BLOB_STORE_ID);
    invalid.checksum_sample_percent = 150.0;
    assert!(matches!(
        runs.create(&invalid).await,
        Err(StoreError::Conflict(_))
    ));

    let present = blob_record_at("present", b"1234", T0);
    let missing = blob_record_at("missing", b"1234", T0);
    let wrong_size = blob_record_at("wrong-size", b"1234", T0);
    for blob in [&present, &missing, &wrong_size] {
        env.store.blobs().insert(blob).await.unwrap();
    }
    let old = T0 - time::Duration::days(2);
    let fresh = T0 + time::Duration::minutes(1);
    runs.stage_listing(
        run.id,
        &[
            ("present".to_owned(), 4, old),
            ("wrong-size".to_owned(), 9, old),
            ("stray-old".to_owned(), 1, old),
            ("stray-new".to_owned(), 1, fresh),
        ],
    )
    .await
    .unwrap();
    // Staging a page twice overwrites the same keys.
    runs.stage_listing(run.id, &[("present".to_owned(), 4, old)])
        .await
        .unwrap();
    assert!(matches!(
        runs.stage_listing(VerifyRunId::generate(), &[]).await,
        Err(StoreError::Conflict(_))
    ));

    let rows = runs.missing(run.id, BLOB_STORE_ID, None, 10).await.unwrap();
    assert_eq!(
        rows.iter().map(|b| b.key.as_str()).collect::<Vec<_>>(),
        ["missing"]
    );
    assert!(
        runs.missing(run.id, BLOB_STORE_ID, Some(missing.id), 10)
            .await
            .unwrap()
            .is_empty()
    );
    let orphans = runs
        .orphans(run.id, BLOB_STORE_ID, T0, None, 10)
        .await
        .unwrap();
    assert_eq!(
        orphans,
        ["stray-old"],
        "a young object may belong to a publish in flight"
    );
    assert!(
        runs.orphans(run.id, BLOB_STORE_ID, T0, Some("stray-old".to_owned()), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        runs.orphans(run.id, BLOB_STORE_ID, T0, None, 0)
            .await
            .unwrap()
            .len(),
        0
    );
    let mismatches = runs
        .size_mismatches(run.id, BLOB_STORE_ID, None, 10)
        .await
        .unwrap();
    assert_eq!(mismatches.len(), 1);
    assert_eq!(
        (mismatches[0].0.key.as_str(), mismatches[0].1),
        ("wrong-size", 9)
    );
    assert!(
        runs.size_mismatches(
            run.id,
            BLOB_STORE_ID,
            Some(wrong_size.id.max(present.id).max(missing.id)),
            10
        )
        .await
        .unwrap()
        .is_empty()
    );
    let unstaged = runs
        .size_mismatches(VerifyRunId::generate(), BLOB_STORE_ID, None, 10)
        .await
        .unwrap();
    assert!(unstaged.is_empty());
    runs.drop_listing(run.id).await.unwrap();
    runs.drop_listing(run.id).await.unwrap();
    let after_drop = runs.missing(run.id, BLOB_STORE_ID, None, 10).await.unwrap();
    assert_eq!(
        after_drop.len(),
        3,
        "nothing is staged, so every row looks missing"
    );
}

#[tokio::test]
async fn verification_findings_are_capped_and_runs_are_listed_newest_first() {
    let env = env();
    let runs = env.store.verify_runs();
    let (first, second) = (run_for(BLOB_STORE_ID), run_for(BlobStoreId::generate()));
    runs.create(&first).await.unwrap();
    runs.create(&second).await.unwrap();
    let all = runs.list(None, &page(10)).await.unwrap();
    assert_eq!(
        all.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        [second.id, first.id]
    );
    let of_store = runs.list(Some(BLOB_STORE_ID), &page(10)).await.unwrap();
    assert_eq!(of_store.items.len(), 1);
    assert_eq!(
        runs.get(first.id).await.unwrap().map(|r| r.id),
        Some(first.id)
    );
    assert!(runs.get(VerifyRunId::generate()).await.unwrap().is_none());

    let mut counts = VerifyCounts::default();
    counts.missing = 3;
    runs.update(first.id, &counts).await.unwrap();
    runs.finish(first.id, TaskRunStatus::Succeeded, &counts, env.clock_now())
        .await
        .unwrap();
    let stored = runs.get(first.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.status, stored.counts.missing),
        (TaskRunStatus::Succeeded, 3)
    );
    assert!(stored.finished_at.is_some());
    assert!(matches!(
        runs.update(VerifyRunId::generate(), &counts).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        runs.finish(VerifyRunId::generate(), TaskRunStatus::Failed, &counts, T0)
            .await,
        Err(StoreError::NotFound(_))
    ));

    let finding = |seq: u32| VerifyFinding {
        run_id: first.id,
        seq,
        kind: VerifyFindingKind::Missing,
        blob_id: None,
        key: format!("k{seq}"),
        detail: String::new(),
    };
    let many: Vec<VerifyFinding> = (0..10_001).map(finding).collect();
    runs.save_findings(&many).await.unwrap();
    let kept = runs.list_findings(first.id, &page(1000)).await.unwrap();
    assert_eq!(kept.items.len(), 1000);
    assert_eq!(kept.items[0].seq, 0);
    let streamed: Vec<VerifyFinding> = {
        let mut after = None;
        let mut all = Vec::new();
        loop {
            let chunk = runs
                .list_findings(
                    first.id,
                    &rustryx_core::ports::PageRequest { limit: 1000, after },
                )
                .await
                .unwrap();
            all.extend(chunk.items);
            match chunk.next {
                Some(token) => after = Some(token),
                None => break,
            }
        }
        all
    };
    assert_eq!(
        streamed.len(),
        10_000,
        "the findings are capped, the counters are not"
    );
    assert!(matches!(
        runs.save_findings(&[finding(5)]).await,
        Err(StoreError::Conflict(_))
    ));
    let mut stranger = finding(1);
    stranger.run_id = VerifyRunId::generate();
    assert!(matches!(
        runs.save_findings(&[stranger]).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        runs.list_findings(VerifyRunId::generate(), &page(1)).await,
        Err(StoreError::NotFound(_))
    ));
}

// statistics

#[tokio::test]
async fn store_statistics_are_replaced_per_store_and_listed_in_store_order() {
    let env = env();
    let (first, second) = two_stores(&env.store).await;
    let stats = env.store.store_stats();
    assert!(stats.get(first.id).await.unwrap().is_none());
    let row = |store: BlobStoreId, used: u64| BlobStoreStats {
        blob_store_id: store,
        used_bytes: used,
        blob_count: 1,
        free_bytes: Some(10),
        total_bytes: None,
        refreshed_at: T0,
    };
    stats.upsert(&row(second.id, 5)).await.unwrap();
    stats.upsert(&row(first.id, 7)).await.unwrap();
    stats.upsert(&row(first.id, 8)).await.unwrap();
    assert_eq!(stats.get(first.id).await.unwrap().unwrap().used_bytes, 8);
    let listed = stats.list().await.unwrap();
    assert_eq!(listed.len(), 2);
    let ids: Vec<BlobStoreId> = listed.iter().map(|s| s.blob_store_id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    // Deleting a store drops its row.
    env.store.blob_stores().delete(first.id).await.unwrap();
    assert!(stats.get(first.id).await.unwrap().is_none());
}
