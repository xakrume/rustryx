//! Quotas, immutability rules, holds, cleanup and retention policies, the lifecycle engine's
//! reads and deletions, run history, garbage collection history.

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

use std::time::Duration;

use common::{Ports, T0, asset, env, hosted, page, project};
use futures::StreamExt;
use rustryx_core::audit::ActorRef;
use rustryx_core::error::StoreError;
use rustryx_core::ids::{
    CleanupPolicyId, ComponentId, GcRunId, ImmutabilityRuleId, LifecycleHoldId, LifecycleRunId,
    ProjectId, RepositoryId, RetentionPolicyId, UserId,
};
use rustryx_core::lifecycle::{
    CleanupCriteria, CleanupPolicy, GcCounts, GcOptions, GcRun, GcSample, HoldKind, HoldTarget,
    ImmutabilityRule, ItemDecision, ItemKind, LifecycleCounts, LifecycleHold, LifecycleItem,
    LifecycleKind, LifecycleRun, NameSelector, PolicyScope, RetentionPolicy, RetentionRule,
    RetentionTemplate,
};
use rustryx_core::model::{NewComponent, RepoFormat, WriteOptions};
use rustryx_core::ports::{DeleteResult, LifecycleScope, PageRequest, PolicyRef};
use rustryx_core::quota::QuotaScope;
use rustryx_core::tasks::{TaskRunStatus, TaskTrigger};

fn actor() -> ActorRef {
    ActorRef::system("test")
}

fn selector(globs: &[&str]) -> NameSelector {
    NameSelector::matching(globs.iter().map(|g| (*g).to_owned()).collect()).unwrap()
}

fn rule(scope: PolicyScope, glob: &str) -> ImmutabilityRule {
    ImmutabilityRule::new(
        ImmutabilityRuleId::generate(),
        scope,
        "frozen",
        ItemKind::Path,
        selector(&[glob]),
        T0,
    )
}

fn hold(target: HoldTarget) -> LifecycleHold {
    LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Legal,
        target,
        "reason",
        T0,
    )
}

fn cleanup(name: &str) -> CleanupPolicy {
    let mut criteria = CleanupCriteria::default();
    criteria.max_age_days = Some(30);
    CleanupPolicy::new(CleanupPolicyId::generate(), name, criteria, T0)
}

fn retention(scope: PolicyScope) -> RetentionPolicy {
    let rule = RetentionRule {
        template: RetentionTemplate::KeepLatestVersions { count: 3 },
        repository: NameSelector::everything(),
        item: NameSelector::everything(),
        include_untagged: false,
    };
    RetentionPolicy::new(
        RetentionPolicyId::generate(),
        scope,
        "keep three",
        vec![rule],
        T0,
    )
}

async fn component(
    store: &rustryx_testkit::MemStore,
    repo: RepositoryId,
    name: &str,
    version: &str,
) -> rustryx_core::model::Component {
    let mut new = NewComponent::new(repo, RepoFormat::Raw, name);
    new.version = Some(version.to_owned());
    store.components().find_or_create(&new).await.unwrap()
}

// quotas

#[tokio::test]
async fn a_scope_without_limit_or_recount_has_no_quota_row() {
    let env = env();
    let repo = hosted(&env.store, "plain").await;
    let scope = QuotaScope::Repository(repo.id);
    assert!(env.store.quotas().get(scope).await.unwrap().is_none());
    let unlimited = env.store.quotas().headroom(repo.id).await.unwrap();
    assert!(unlimited.bytes.is_none() && unlimited.binding.is_none());
    // A recount creates the counter row without a limit.
    env.store
        .assets()
        .publish_with(&asset(repo.id, "a", b"12345"), &WriteOptions::new())
        .await
        .unwrap();
    let (before, after) = env.store.quotas().recount(scope).await.unwrap();
    assert_eq!((before, after), (5, 5));
    let usage = env.store.quotas().get(scope).await.unwrap().unwrap();
    assert_eq!(
        (usage.used_bytes, usage.object_count, usage.limit_bytes),
        (5, 1, None)
    );
    assert_eq!(usage.recounted_at, Some(env.clock_now()));
    assert!(
        env.store
            .quotas()
            .get(QuotaScope::Repository(RepositoryId::generate()))
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        env.store
            .quotas()
            .recount(QuotaScope::Project(ProjectId::generate()))
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        env.store.quotas().headroom(RepositoryId::generate()).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn project_quota_limits_are_versioned_and_validated() {
    let env = env();
    let created = project(&env.store, "limits").await;
    let scope = QuotaScope::Project(created.id);
    let quotas = env.store.quotas();
    for bad in [0_u8, 101] {
        let error = quotas
            .set_limit(scope, Some(1), bad, None, &actor())
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::Invalid(_)), "{bad}");
    }
    quotas
        .set_limit(scope, Some(100), 70, Some(0), &actor())
        .await
        .unwrap();
    let usage = quotas.get(scope).await.unwrap().unwrap();
    assert_eq!((usage.limit_bytes, usage.warn_percent), (Some(100), 70));
    quotas
        .set_limit(scope, Some(200), 70, Some(1), &actor())
        .await
        .unwrap();
    let stale = quotas
        .set_limit(scope, None, 70, Some(1), &actor())
        .await
        .unwrap_err();
    assert!(matches!(stale, StoreError::Conflict(_)));
    // No expectation at all just writes; `None` limit is unlimited.
    quotas
        .set_limit(scope, None, 80, None, &actor())
        .await
        .unwrap();
    assert_eq!(quotas.get(scope).await.unwrap().unwrap().limit_bytes, None);
    let ghost = QuotaScope::Project(ProjectId::generate());
    assert!(matches!(
        quotas.set_limit(ghost, Some(1), 80, None, &actor()).await,
        Err(StoreError::NotFound(_))
    ));
    let missing_repo = QuotaScope::Repository(RepositoryId::generate());
    assert!(matches!(
        quotas
            .set_limit(missing_repo, Some(1), 80, None, &actor())
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn quota_listing_covers_scopes_with_a_counter_in_key_order() {
    let env = env();
    let created = project(&env.store, "listed").await;
    let repo = hosted(&env.store, "listed-repo").await;
    let untouched = hosted(&env.store, "untouched").await;
    env.store
        .quotas()
        .set_limit(
            QuotaScope::Project(created.id),
            Some(10),
            80,
            None,
            &actor(),
        )
        .await
        .unwrap();
    env.store
        .quotas()
        .set_limit(
            QuotaScope::Repository(repo.id),
            Some(10),
            90,
            None,
            &actor(),
        )
        .await
        .unwrap();
    let listed = env.store.quotas().list(&page(10)).await.unwrap();
    let scopes: Vec<QuotaScope> = listed.items.iter().map(|u| u.scope).collect();
    assert_eq!(
        scopes,
        [
            QuotaScope::Project(created.id),
            QuotaScope::Repository(repo.id)
        ]
    );
    assert!(!scopes.contains(&QuotaScope::Repository(untouched.id)));
    assert_eq!(listed.items[1].warn_percent, 90);
    let first = env.store.quotas().list(&page(1)).await.unwrap();
    assert!(first.next.is_some());
}

// immutability rules

#[tokio::test]
async fn immutability_rules_validate_count_and_version() {
    let env = env();
    let repo = hosted(&env.store, "rules").await;
    let scope = PolicyScope::Repository(repo.id);
    let rules = env.store.immutability();
    let first = rule(scope, "release/**");
    rules.create(&first).await.unwrap();
    assert!(matches!(
        rules.create(&first).await,
        Err(StoreError::Conflict(_))
    ));
    // The system scope holds no rule, and the scope must exist.
    assert!(matches!(
        rules.create(&rule(PolicyScope::System, "x")).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        rules
            .create(&rule(PolicyScope::Project(ProjectId::generate()), "x"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    for index in 1..15 {
        rules
            .create(&rule(scope, &format!("p{index}/**")))
            .await
            .unwrap();
    }
    let sixteenth = rule(scope, "one-too-many/**");
    assert!(
        matches!(rules.create(&sixteenth).await, Err(StoreError::Conflict(_))),
        "at most 15 per scope"
    );

    let mut edited = first.clone();
    edited.name = "renamed".to_owned();
    edited.scope = PolicyScope::System;
    rules.update(&edited, 1).await.unwrap();
    let stored = rules.get(first.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.version, stored.scope, stored.name.as_str()),
        (2, scope, "renamed")
    );
    assert!(matches!(
        rules.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut missing = first.clone();
    missing.id = ImmutabilityRuleId::generate();
    assert!(matches!(
        rules.update(&missing, 1).await,
        Err(StoreError::NotFound(_))
    ));
    let mut invalid = stored.clone();
    invalid.name = String::new();
    assert!(matches!(
        rules.update(&invalid, 2).await,
        Err(StoreError::Conflict(_))
    ));

    let listed = rules.list(scope, &page(100)).await.unwrap();
    assert_eq!(listed.items.len(), 15);
    rules.delete(first.id).await.unwrap();
    assert!(matches!(
        rules.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        rules
            .covering_rule(RepositoryId::generate(), ItemKind::Path, "x")
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn the_editor_preview_lists_existing_items_a_rule_would_cover() {
    let env = env();
    let created = project(&env.store, "previewed").await;
    let repo = hosted(&env.store, "preview-repo").await;
    let other = hosted(&env.store, "preview-other").await;
    env.store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    for path in ["release/a", "release/b", "snapshot/c"] {
        env.store
            .assets()
            .publish_with(&asset(repo.id, path, b"x"), &WriteOptions::new())
            .await
            .unwrap();
    }
    env.store
        .assets()
        .publish_with(&asset(other.id, "release/z", b"x"), &WriteOptions::new())
        .await
        .unwrap();
    component(&env.store, repo.id, "lib", "1.0").await;
    let by_path = rule(PolicyScope::Project(created.id), "release/**");
    let items = env
        .store
        .immutability()
        .preview(&by_path, 10)
        .await
        .unwrap();
    assert_eq!(
        items,
        ["release/a", "release/b"],
        "only the repositories of the project"
    );
    assert_eq!(
        env.store
            .immutability()
            .preview(&by_path, 1)
            .await
            .unwrap()
            .len(),
        1
    );
    let mut by_version = rule(PolicyScope::Repository(repo.id), "1.*");
    by_version.item_kind = ItemKind::Version;
    assert_eq!(
        env.store
            .immutability()
            .preview(&by_version, 10)
            .await
            .unwrap(),
        ["1.0"]
    );
    let mut by_tag = by_path.clone();
    by_tag.item_kind = ItemKind::Tag;
    assert!(
        env.store
            .immutability()
            .preview(&by_tag, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let mut narrowed = by_path.clone();
    narrowed.repository = selector(&["preview-o*"]);
    assert!(
        env.store
            .immutability()
            .preview(&narrowed, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

// holds

#[tokio::test]
async fn holds_are_listed_released_once_and_found_by_component() {
    let env = env();
    let created = project(&env.store, "held").await;
    let repo = hosted(&env.store, "held-repo").await;
    env.store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    let target = component(&env.store, repo.id, "pkg", "1").await;
    let on_component = hold(HoldTarget::Component(target.id));
    let on_repo = hold(HoldTarget::Repository(repo.id));
    let on_project = hold(HoldTarget::Project(created.id));
    for h in [&on_component, &on_repo, &on_project] {
        env.store.holds().create(h).await.unwrap();
    }
    assert!(matches!(
        env.store.holds().create(&on_repo).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        env.store
            .holds()
            .create(&hold(HoldTarget::Component(ComponentId::generate())))
            .await,
        Err(StoreError::Conflict(_))
    ));
    let mut no_reason = hold(HoldTarget::Repository(repo.id));
    no_reason.reason = String::new();
    assert!(matches!(
        env.store.holds().create(&no_reason).await,
        Err(StoreError::Conflict(_))
    ));

    let active = env.store.holds().active_for(target.id).await.unwrap();
    assert_eq!(active.len(), 3);
    assert!(
        env.store
            .holds()
            .active_for(ComponentId::generate())
            .await
            .unwrap()
            .is_empty()
    );
    let user = Some(UserId::generate());
    let at = env.clock_now();
    env.store
        .holds()
        .release(on_repo.id, user, at)
        .await
        .unwrap();
    env.store
        .holds()
        .release(on_repo.id, None, at + time::Duration::days(1))
        .await
        .unwrap();
    let released = env.store.holds().get(on_repo.id).await.unwrap().unwrap();
    assert_eq!(
        (released.released_at, released.released_by),
        (Some(at), user),
        "the first release stays"
    );
    assert!(matches!(
        env.store
            .holds()
            .release(LifecycleHoldId::generate(), None, at)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        env.store.holds().active_for(target.id).await.unwrap().len(),
        2
    );

    let all = env
        .store
        .holds()
        .list(None, false, &page(10))
        .await
        .unwrap();
    assert_eq!(all.items.len(), 3);
    let only_active = env.store.holds().list(None, true, &page(10)).await.unwrap();
    assert_eq!(only_active.items.len(), 2);
    let mine = env
        .store
        .holds()
        .list(Some(HoldTarget::Component(target.id)), false, &page(10))
        .await
        .unwrap();
    assert_eq!(mine.items.len(), 1);
}

// cleanup and retention policies

#[tokio::test]
async fn cleanup_policies_are_unique_versioned_and_protected_while_attached() {
    let env = env();
    let policies = env.store.cleanup_policies();
    let first = cleanup("weekly");
    policies.create(&first).await.unwrap();
    assert!(matches!(
        policies.create(&first).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        policies.create(&cleanup("weekly")).await,
        Err(StoreError::Conflict(_))
    ));
    let empty = CleanupPolicy::new(
        CleanupPolicyId::generate(),
        "empty",
        CleanupCriteria::default(),
        T0,
    );
    assert!(
        matches!(policies.create(&empty).await, Err(StoreError::Conflict(_))),
        "a criterion is required"
    );
    policies.create(&cleanup("alpha")).await.unwrap();
    let names: Vec<String> = policies
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["alpha", "weekly"]);

    let mut edited = first.clone();
    edited.description = "kept".to_owned();
    policies.update(&edited, 1).await.unwrap();
    assert_eq!(policies.get(first.id).await.unwrap().unwrap().version, 2);
    assert!(matches!(
        policies.update(&edited, 1).await,
        Err(StoreError::Conflict(_))
    ));
    let mut clash = edited.clone();
    clash.name = "alpha".to_owned();
    assert!(matches!(
        policies.update(&clash, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut bad = edited.clone();
    bad.criteria = CleanupCriteria::default();
    assert!(matches!(
        policies.update(&bad, 2).await,
        Err(StoreError::Conflict(_))
    ));
    let mut ghost = edited.clone();
    ghost.id = CleanupPolicyId::generate();
    assert!(matches!(
        policies.update(&ghost, 1).await,
        Err(StoreError::NotFound(_))
    ));

    let mut repo = rustryx_testkit::fixtures::repository(
        "cleaned",
        RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repo.cleanup_policy_ids = vec![first.id];
    env.store.repos().create(&repo).await.unwrap();
    assert_eq!(
        policies.repositories_using(first.id).await.unwrap(),
        [repo.id]
    );
    assert!(matches!(
        policies.delete(first.id).await,
        Err(StoreError::Conflict(_))
    ));
    repo.cleanup_policy_ids.clear();
    env.store.repos().update(&repo).await.unwrap();
    policies.delete(first.id).await.unwrap();
    assert!(matches!(
        policies.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_scope_holds_one_retention_policy_replaced_only_against_its_version() {
    let env = env();
    let created = project(&env.store, "retained").await;
    let scope = PolicyScope::Project(created.id);
    let policies = env.store.retention();
    let first = retention(scope);
    policies.upsert(&first, None).await.unwrap();
    assert!(matches!(
        policies.upsert(&retention(scope), None).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        policies.upsert(&retention(scope), Some(7)).await,
        Err(StoreError::Conflict(_))
    ));
    let mut second = retention(scope);
    second.allow_delete_last_version = true;
    policies.upsert(&second, Some(1)).await.unwrap();
    let stored = policies.get_for_scope(scope).await.unwrap().unwrap();
    assert_eq!(
        (stored.id, stored.version, stored.allow_delete_last_version),
        (first.id, 2, true)
    );
    assert!(policies.get(first.id).await.unwrap().is_some());
    // No policy yet for another scope, and a version cannot be expected against nothing.
    let repo = hosted(&env.store, "retention-repo").await;
    let repo_scope = PolicyScope::Repository(repo.id);
    assert!(policies.get_for_scope(repo_scope).await.unwrap().is_none());
    assert!(matches!(
        policies.upsert(&retention(repo_scope), Some(1)).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        policies.upsert(&retention(PolicyScope::System), None).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        policies
            .upsert(
                &retention(PolicyScope::Project(ProjectId::generate())),
                None
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        policies.upsert(&first, None).await,
        Err(StoreError::Conflict(_))
    ));
    policies.delete(first.id).await.unwrap();
    assert!(matches!(
        policies.delete(first.id).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        policies
            .get_for_scope(PolicyScope::System)
            .await
            .unwrap()
            .is_none()
    );
}

// the engine

#[tokio::test]
async fn candidates_come_in_engine_order_one_package_at_a_time() {
    let env = env();
    let created = project(&env.store, "engine").await;
    let repo = hosted(&env.store, "engine-repo").await;
    env.store
        .projects()
        .attach_repository(created.id, repo.id)
        .await
        .unwrap();
    let other = hosted(&env.store, "unattached").await;
    for version in ["1.0", "2.0", "3.0"] {
        env.clock.advance(Duration::from_secs(60));
        let c = component(&env.store, repo.id, "zeta", version).await;
        let mut file = asset(repo.id, &format!("zeta/{version}"), b"12345");
        file.component_id = Some(c.id);
        env.store
            .assets()
            .publish_with(&file, &WriteOptions::new())
            .await
            .unwrap();
    }
    component(&env.store, repo.id, "alpha", "1").await;
    component(&env.store, other.id, "outside", "1").await;

    let scope = LifecycleScope::Project(created.id);
    let all = env
        .store
        .lifecycle()
        .candidates(scope, &page(10))
        .await
        .unwrap();
    let order: Vec<(String, Option<String>)> = all
        .items
        .iter()
        .map(|c| (c.name.clone(), c.version.clone()))
        .collect();
    assert_eq!(
        order,
        [
            ("alpha".to_owned(), Some("1".to_owned())),
            ("zeta".to_owned(), Some("3.0".to_owned())),
            ("zeta".to_owned(), Some("2.0".to_owned())),
            ("zeta".to_owned(), Some("1.0".to_owned())),
        ],
        "by name, then newest version first"
    );
    let zeta = &all.items[1];
    assert_eq!(
        (zeta.size_bytes, zeta.repository.as_str()),
        (5, "engine-repo")
    );
    assert!(zeta.tags.is_empty() && !zeta.is_referrer);
    let mut after = None;
    let mut paged = Vec::new();
    loop {
        let chunk = env
            .store
            .lifecycle()
            .candidates(
                LifecycleScope::Repository(repo.id),
                &PageRequest { limit: 3, after },
            )
            .await
            .unwrap();
        paged.extend(chunk.items.into_iter().map(|c| c.component_id));
        match chunk.next {
            Some(token) => after = Some(token),
            None => break,
        }
    }
    assert_eq!(paged.len(), 4);
    for missing in [
        LifecycleScope::Repository(RepositoryId::generate()),
        LifecycleScope::Project(ProjectId::generate()),
    ] {
        assert!(matches!(
            env.store.lifecycle().candidates(missing, &page(10)).await,
            Err(StoreError::NotFound(_))
        ));
    }
}

#[tokio::test]
async fn apply_deletions_skips_what_is_protected_and_records_what_it_did() {
    let env = env();
    let repo = hosted(&env.store, "applied").await;
    let plain = component(&env.store, repo.id, "plain", "1").await;
    let frozen = component(&env.store, repo.id, "frozen", "1.0").await;
    let held = component(&env.store, repo.id, "held", "9").await;
    let mut version_rule = rule(PolicyScope::Repository(repo.id), "1.*");
    version_rule.item_kind = ItemKind::Version;
    env.store
        .immutability()
        .create(&version_rule)
        .await
        .unwrap();
    let on_held = hold(HoldTarget::Component(held.id));
    env.store.holds().create(&on_held).await.unwrap();
    let mut file = asset(repo.id, "plain/file", b"1234");
    file.component_id = Some(plain.id);
    env.store
        .assets()
        .publish_with(&file, &WriteOptions::new())
        .await
        .unwrap();

    let run = LifecycleRun::new(LifecycleRunId::generate(), LifecycleKind::Cleanup, false, 1);
    env.store.lifecycle_runs().create(&run).await.unwrap();
    let batch = [plain.id, frozen.id, held.id, ComponentId::generate()];
    let results = env
        .store
        .lifecycle()
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
    assert!(
        env.store
            .components()
            .get(plain.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .assets()
            .get_by_path(repo.id, "plain/file")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .components()
            .get(frozen.id)
            .await
            .unwrap()
            .is_some()
    );
    let items = env
        .store
        .lifecycle_runs()
        .list_items(run.id, &page(10))
        .await
        .unwrap();
    let decisions: Vec<ItemDecision> = items.items.iter().map(|i| i.decision).collect();
    assert_eq!(
        decisions,
        [
            ItemDecision::Delete,
            ItemDecision::ProtectedImmutable,
            ItemDecision::ProtectedHold
        ]
    );
    assert_eq!(items.items[1].reason, version_rule.id.to_string());
    assert_eq!(items.items[2].reason, on_held.id.to_string());

    // An admin run may override immutability, never a legal hold.
    let overridden = env
        .store
        .lifecycle()
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
}

#[tokio::test]
async fn apply_deletions_refuses_oversized_batches_unknown_runs_and_read_only_mode() {
    let env = env();
    let repo = hosted(&env.store, "refusals").await;
    let target = component(&env.store, repo.id, "pkg", "1").await;
    let run = LifecycleRun::new(
        LifecycleRunId::generate(),
        LifecycleKind::Retention,
        false,
        1,
    );
    env.store.lifecycle_runs().create(&run).await.unwrap();
    let too_many: Vec<ComponentId> = (0..501).map(|_| ComponentId::generate()).collect();
    assert!(matches!(
        env.store
            .lifecycle()
            .apply_deletions(run.id, &too_many, &WriteOptions::new())
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        env.store
            .lifecycle()
            .apply_deletions(
                LifecycleRunId::generate(),
                &[target.id],
                &WriteOptions::new()
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    env.store.set_read_only(true, "freeze").unwrap();
    let error = env
        .store
        .lifecycle()
        .apply_deletions(run.id, &[target.id], &WriteOptions::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Conflict(_)),
        "RX007 aborts the whole batch"
    );
    assert!(
        env.store
            .components()
            .get(target.id)
            .await
            .unwrap()
            .is_some()
    );
}

// run history

fn item(run: LifecycleRunId, seq: u64) -> LifecycleItem {
    LifecycleItem {
        run_id: run,
        seq,
        repository: "r".to_owned(),
        component_id: ComponentId::generate(),
        coordinate: format!("pkg:{seq}"),
        tags: Vec::new(),
        size_bytes: seq,
        decision: ItemDecision::Delete,
        reason: "criterion".to_owned(),
    }
}

#[tokio::test]
async fn lifecycle_runs_are_listed_newest_first_filtered_and_gated_by_dry_runs() {
    let env = env();
    let policy = cleanup("gated");
    env.store.cleanup_policies().create(&policy).await.unwrap();
    let repo = hosted(&env.store, "gated-repo").await;
    let make = |dry: bool, version: i64| {
        let mut run = LifecycleRun::new(
            LifecycleRunId::generate(),
            LifecycleKind::Cleanup,
            dry,
            version,
        );
        run.cleanup_policy_id = Some(policy.id);
        run.repository_id = Some(repo.id);
        run
    };
    let runs = env.store.lifecycle_runs();
    let mut dry = make(true, 2);
    dry.started_at = Some(env.clock_now());
    runs.create(&dry).await.unwrap();
    assert!(matches!(
        runs.create(&dry).await,
        Err(StoreError::Conflict(_))
    ));
    let mut counts = LifecycleCounts::default();
    counts.evaluated = 10;
    runs.update_progress(dry.id, &counts).await.unwrap();
    env.clock.advance(Duration::from_secs(60));
    runs.finish(
        dry.id,
        TaskRunStatus::Succeeded,
        &counts,
        true,
        env.clock_now(),
    )
    .await
    .unwrap();
    let stored = runs.get(dry.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.status, stored.truncated, stored.counts.evaluated),
        (TaskRunStatus::Succeeded, true, 10)
    );
    assert!(stored.finished_at.is_some());

    let mut real = make(false, 2);
    real.started_at = Some(env.clock_now());
    runs.create(&real).await.unwrap();
    let mut retention_run = LifecycleRun::new(
        LifecycleRunId::generate(),
        LifecycleKind::Retention,
        true,
        1,
    );
    retention_run.retention_policy_id = Some(RetentionPolicyId::generate());
    runs.create(&retention_run).await.unwrap();

    let everything = runs.list(None, None, &page(10)).await.unwrap();
    assert_eq!(
        everything.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        [retention_run.id, real.id, dry.id]
    );
    let cleanups = runs
        .list(Some(LifecycleKind::Cleanup), None, &page(10))
        .await
        .unwrap();
    assert_eq!(cleanups.items.len(), 2);
    let of_policy = runs
        .list(
            None,
            Some(PolicyRef::Cleanup(policy.id, Some(repo.id))),
            &page(10),
        )
        .await
        .unwrap();
    assert_eq!(of_policy.items.len(), 2);
    let of_other_repo = runs
        .list(
            None,
            Some(PolicyRef::Cleanup(
                policy.id,
                Some(RepositoryId::generate()),
            )),
            &page(10),
        )
        .await
        .unwrap();
    assert!(of_other_repo.items.is_empty());
    let of_retention = runs
        .list(
            None,
            Some(PolicyRef::Retention(
                retention_run.retention_policy_id.unwrap(),
            )),
            &page(10),
        )
        .await
        .unwrap();
    assert_eq!(of_retention.items.len(), 1);

    let window = Duration::from_hours(336);
    let gate = runs
        .last_dry_run(PolicyRef::Cleanup(policy.id, None), 2, window)
        .await
        .unwrap();
    assert_eq!(gate.map(|r| r.id), Some(dry.id));
    assert!(
        runs.last_dry_run(PolicyRef::Cleanup(policy.id, None), 3, window)
            .await
            .unwrap()
            .is_none(),
        "another version"
    );
    env.clock.advance(Duration::from_hours(360));
    assert!(
        runs.last_dry_run(PolicyRef::Cleanup(policy.id, None), 2, window)
            .await
            .unwrap()
            .is_none(),
        "too old"
    );
    assert!(matches!(
        runs.update_progress(LifecycleRunId::generate(), &counts)
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        runs.finish(
            LifecycleRunId::generate(),
            TaskRunStatus::Failed,
            &counts,
            false,
            T0
        )
        .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn must_not_open_the_gate_with_a_failed_dry_run() {
    let env = env();
    let policy = cleanup("failed-gate");
    let mut run = LifecycleRun::new(LifecycleRunId::generate(), LifecycleKind::Cleanup, true, 1);
    run.cleanup_policy_id = Some(policy.id);
    run.started_at = Some(env.clock_now());
    env.store.lifecycle_runs().create(&run).await.unwrap();
    env.store
        .lifecycle_runs()
        .finish(
            run.id,
            TaskRunStatus::Failed,
            &LifecycleCounts::default(),
            false,
            env.clock_now(),
        )
        .await
        .unwrap();
    let gate = env
        .store
        .lifecycle_runs()
        .last_dry_run(
            PolicyRef::Cleanup(policy.id, None),
            1,
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    assert!(gate.is_none());
}

#[tokio::test]
async fn run_items_are_stored_capped_streamed_and_purged_with_their_run() {
    let env = env();
    let runs = env.store.lifecycle_runs();
    let run = LifecycleRun::new(LifecycleRunId::generate(), LifecycleKind::Cleanup, true, 1);
    runs.create(&run).await.unwrap();
    let batch: Vec<LifecycleItem> = (0..5).map(|seq| item(run.id, seq)).collect();
    assert!(!runs.save_items(run.id, &batch).await.unwrap());
    assert!(matches!(
        runs.save_items(run.id, &batch[..1]).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        runs.save_items(LifecycleRunId::generate(), &batch).await,
        Err(StoreError::NotFound(_))
    ));
    let first = runs.list_items(run.id, &page(2)).await.unwrap();
    assert_eq!(
        first.items.iter().map(|i| i.seq).collect::<Vec<_>>(),
        [0, 1]
    );
    let streamed: Vec<u64> = runs
        .stream_items(run.id)
        .map(|item| item.unwrap().seq)
        .collect()
        .await;
    assert_eq!(streamed, [0, 1, 2, 3, 4]);
    assert!(matches!(
        runs.list_items(LifecycleRunId::generate(), &page(1)).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        runs.stream_items(LifecycleRunId::generate())
            .collect::<Vec<_>>()
            .await
            .is_empty()
    );

    // The cap of 100000 items per run: the list is cut and the caller is told.
    let big = LifecycleRun::new(LifecycleRunId::generate(), LifecycleKind::Cleanup, true, 1);
    runs.create(&big).await.unwrap();
    let many: Vec<LifecycleItem> = (0..100_001).map(|seq| item(big.id, seq)).collect();
    assert!(runs.save_items(big.id, &many).await.unwrap());
    let kept = runs.stream_items(big.id).count().await;
    assert_eq!(kept, 100_000);

    runs.finish(
        run.id,
        TaskRunStatus::Succeeded,
        &LifecycleCounts::default(),
        false,
        T0,
    )
    .await
    .unwrap();
    env.clock.advance(Duration::from_secs(1));
    let removed = runs.purge(env.clock_now()).await.unwrap();
    assert_eq!(removed, 1);
    assert!(runs.get(run.id).await.unwrap().is_none());
    assert!(
        runs.get(big.id).await.unwrap().is_some(),
        "an unfinished run is never purged"
    );
}

// garbage collection history

fn gc_run(trigger: TaskTrigger) -> GcRun {
    GcRun::new(GcRunId::generate(), trigger, GcOptions::new(3600))
}

#[tokio::test]
async fn gc_runs_track_progress_stop_requests_samples_and_the_manual_start() {
    let env = env();
    let runs = env.store.gc_runs();
    let manual = gc_run(TaskTrigger::Manual);
    let scheduled = gc_run(TaskTrigger::Schedule);
    runs.create(&manual).await.unwrap();
    assert!(matches!(
        runs.create(&manual).await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        runs.last_manual_start().await.unwrap(),
        Some(env.clock_now()),
        "a queued manual run counts from its creation"
    );
    env.clock.advance(Duration::from_secs(30));
    runs.create(&scheduled).await.unwrap();
    assert_eq!(runs.last_manual_start().await.unwrap(), Some(T0));
    let listed = runs.list(&page(10)).await.unwrap();
    assert_eq!(
        listed.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        [scheduled.id, manual.id]
    );

    let mut counts = GcCounts::default();
    counts.deleted = 4;
    runs.update_progress(manual.id, &counts, &serde_json::json!({"default": 4}))
        .await
        .unwrap();
    let stored = runs.get(manual.id).await.unwrap().unwrap();
    assert_eq!(
        (stored.counts.deleted, stored.per_store["default"].clone()),
        (4, serde_json::json!(4))
    );
    assert!(!runs.stop_requested(manual.id).await.unwrap());
    assert!(runs.request_stop(manual.id).await.unwrap());
    assert!(runs.stop_requested(manual.id).await.unwrap());
    runs.finish(manual.id, TaskRunStatus::Cancelled, env.clock_now())
        .await
        .unwrap();
    assert!(
        !runs.request_stop(manual.id).await.unwrap(),
        "a finished run cannot be stopped"
    );
    for missing in [GcRunId::generate()] {
        assert!(matches!(
            runs.stop_requested(missing).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            runs.request_stop(missing).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            runs.update_progress(missing, &counts, &serde_json::json!({}))
                .await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            runs.finish(missing, TaskRunStatus::Failed, T0).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            runs.list_samples(missing, &page(1)).await,
            Err(StoreError::NotFound(_))
        ));
    }

    let sample = |key: &str| GcSample {
        run_id: manual.id,
        blob_store: "default".to_owned(),
        key: key.to_owned(),
        size_bytes: 1,
        orphaned_at: T0,
    };
    let rows: Vec<GcSample> = (0..1001).map(|i| sample(&format!("k{i}"))).collect();
    runs.save_samples(&rows).await.unwrap();
    let kept = runs.list_samples(manual.id, &page(1000)).await.unwrap();
    assert_eq!(kept.items.len(), 1000, "the sample is capped at 1000 rows");
    assert!(kept.next.is_none());
    let mut stranger = sample("x");
    stranger.run_id = GcRunId::generate();
    assert!(matches!(
        runs.save_samples(&[stranger]).await,
        Err(StoreError::Conflict(_))
    ));

    env.clock.advance(Duration::from_secs(60));
    let purged = runs.purge(env.clock_now()).await.unwrap();
    assert_eq!(purged, 1);
    assert!(runs.get(manual.id).await.unwrap().is_none());
    assert!(runs.get(scheduled.id).await.unwrap().is_some());
}
