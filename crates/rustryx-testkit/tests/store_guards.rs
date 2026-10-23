//! The write path of `MemStore`: the guards that PostgreSQL runs as triggers (immutability,
//! holds, quota, read only mode), the write policy, create only mode, `link`, the checked
//! deletes and the outbox events that ride in the same step.

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

use common::{Ports, asset, env, hosted, project, public_project};
use rustryx_core::audit::ActorRef;
use rustryx_core::error::{MutationError, PolicyViolation, StoreError};
use rustryx_core::events::{EventDraft, EventKind};
use rustryx_core::ids::{ComponentId, ImmutabilityRuleId, LifecycleHoldId, RepositoryId};
use rustryx_core::lifecycle::{
    HoldKind, HoldTarget, ImmutabilityRule, ItemKind, LifecycleHold, NameSelector, PolicyScope,
};
use rustryx_core::model::{
    DownloadTick, NewAssetLink, NewComponent, PublishMode, RepoFormat, RepoType, WriteOptions,
    WritePolicy,
};
use rustryx_core::quota::QuotaScope;
use rustryx_testkit::fixtures::{blob_record, repository};
use time::macros::datetime;

fn globs(items: &[&str]) -> NameSelector {
    NameSelector::matching(items.iter().map(|s| (*s).to_owned()).collect()).unwrap()
}

fn path_rule(scope: PolicyScope, glob: &str) -> ImmutabilityRule {
    ImmutabilityRule::new(
        ImmutabilityRuleId::generate(),
        scope,
        "frozen",
        ItemKind::Path,
        globs(&[glob]),
        common::T0,
    )
}

fn legal_hold(target: HoldTarget) -> LifecycleHold {
    LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Legal,
        target,
        "litigation",
        common::T0,
    )
}

fn actor() -> ActorRef {
    ActorRef::system("test")
}

fn violation(error: MutationError) -> PolicyViolation {
    match error {
        MutationError::Violation(violation) => violation,
        other => panic!("expected a policy violation, got {other:?}"),
    }
}

fn draft(kind: EventKind) -> EventDraft {
    EventDraft::new(kind, actor(), serde_json::json!({}))
}

// publish_with: write policy and create only

#[tokio::test]
async fn publish_with_creates_then_replaces_and_reports_both() {
    let env = env();
    let repo = hosted(&env.store, "files").await;
    let first = asset(repo.id, "a.bin", b"one");
    let created = env
        .store
        .assets()
        .publish_with(&first, &WriteOptions::new())
        .await
        .unwrap();
    assert!(created.created && created.replaced.is_none());
    env.clock.advance(Duration::from_secs(5));
    let second = asset(repo.id, "a.bin", b"two!");
    let replaced = env
        .store
        .assets()
        .publish_with(&second, &WriteOptions::new())
        .await
        .unwrap();
    assert!(!replaced.created);
    assert_eq!(replaced.replaced.map(|b| b.id), Some(first.blob.id));
    assert_eq!(replaced.asset.id, created.asset.id);
}

#[tokio::test]
async fn must_not_replace_a_path_of_an_allow_once_repository() {
    let env = env();
    let mut repo = repository("once", RepoFormat::Raw, RepoType::Hosted);
    repo.write_policy = WritePolicy::AllowOnce;
    env.store.repos().create(&repo).await.unwrap();
    let assets = env.store.assets();
    assets
        .publish_with(&asset(repo.id, "x", b"1"), &WriteOptions::new())
        .await
        .unwrap();
    let again = asset(repo.id, "x", b"2");
    let refused = assets
        .publish_with(&again, &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(
        violation(refused),
        PolicyViolation::WritePolicy(WritePolicy::AllowOnce)
    );
    assert!(
        env.store
            .blobs()
            .get(again.blob.id)
            .await
            .unwrap()
            .is_none()
    );
    // A second path is a first write and goes through.
    assets
        .publish_with(&asset(repo.id, "y", b"3"), &WriteOptions::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn must_not_write_to_a_deny_repository_but_a_proxy_cache_may_fill() {
    let env = env();
    let mut hosted_repo = repository("ro", RepoFormat::Raw, RepoType::Hosted);
    hosted_repo.write_policy = WritePolicy::Deny;
    env.store.repos().create(&hosted_repo).await.unwrap();
    let refused = env
        .store
        .assets()
        .publish_with(&asset(hosted_repo.id, "x", b"1"), &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(
        violation(refused),
        PolicyViolation::WritePolicy(WritePolicy::Deny)
    );
    // The fixture gives proxy repositories `Deny`: the system fills their cache anyway.
    let proxy = repository("cache", RepoFormat::Npm, RepoType::Proxy);
    assert_eq!(proxy.write_policy, WritePolicy::Deny);
    env.store.repos().create(&proxy).await.unwrap();
    env.store
        .assets()
        .publish_with(&asset(proxy.id, "pkg", b"1"), &WriteOptions::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn create_only_refuses_an_existing_path_and_accepts_a_new_one() {
    let env = env();
    let repo = hosted(&env.store, "fills").await;
    let options = WriteOptions::new().with_mode(PublishMode::CreateOnly);
    let assets = env.store.assets();
    assets
        .publish_with(&asset(repo.id, "p", b"1"), &options)
        .await
        .unwrap();
    let again = asset(repo.id, "p", b"2");
    let refused = assets.publish_with(&again, &options).await.unwrap_err();
    assert_eq!(violation(refused), PolicyViolation::AlreadyExists);
    assert!(
        env.store
            .blobs()
            .get(again.blob.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn phase_one_publish_flattens_a_violation_to_conflict() {
    let env = env();
    let mut repo = repository("flat", RepoFormat::Raw, RepoType::Hosted);
    repo.write_policy = WritePolicy::Deny;
    env.store.repos().create(&repo).await.unwrap();
    let error = env
        .store
        .assets()
        .publish(&asset(repo.id, "x", b"1"))
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Conflict(_)), "{error:?}");
}

#[tokio::test]
async fn must_not_reuse_a_blob_id_or_key_for_a_second_publish() {
    let env = env();
    let repo = hosted(&env.store, "keys").await;
    let first = asset(repo.id, "a", b"1");
    env.store
        .assets()
        .publish_with(&first, &WriteOptions::new())
        .await
        .unwrap();
    let mut same_id = asset(repo.id, "b", b"2");
    same_id.blob.id = first.blob.id;
    let mut same_key = asset(repo.id, "c", b"3");
    same_key.blob.key = first.blob.key.clone();
    for clash in [same_id, same_key] {
        let error = env
            .store
            .assets()
            .publish_with(&clash, &WriteOptions::new())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            MutationError::Store(StoreError::Conflict(_))
        ));
    }
}

// immutability

#[tokio::test]
async fn must_not_overwrite_or_delete_an_immutable_path() {
    let env = env();
    let repo = hosted(&env.store, "frozen").await;
    let rule = path_rule(PolicyScope::Repository(repo.id), "release/**");
    env.store.immutability().create(&rule).await.unwrap();
    let assets = env.store.assets();
    // Creation under the rule is fine: only a change of what exists is refused.
    let first = asset(repo.id, "release/1.0/app.bin", b"v1");
    assets
        .publish_with(&first, &WriteOptions::new())
        .await
        .unwrap();

    let again = asset(repo.id, "release/1.0/app.bin", b"v2");
    let refused = assets
        .publish_with(
            &again,
            &WriteOptions::new().with_event(draft(EventKind::Push)),
        )
        .await
        .unwrap_err();
    assert_eq!(
        violation(refused),
        PolicyViolation::Immutable { rule: rule.id }
    );
    // A refusal leaves nothing behind: no blob row, no event, the old content.
    assert!(
        env.store
            .blobs()
            .get(again.blob.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        env.store
            .outbox()
            .read_batch("c", 10)
            .await
            .unwrap()
            .events
            .len(),
        0
    );
    let current = assets
        .get_by_path(repo.id, "release/1.0/app.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.blob_id, first.blob.id);

    let delete = assets
        .delete_by_path_checked(repo.id, "release/1.0/app.bin", &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(
        violation(delete),
        PolicyViolation::Immutable { rule: rule.id }
    );
    // The phase 1 delete is guarded too, and reports a conflict to an N-1 caller.
    let old_style = assets
        .delete_by_path(repo.id, "release/1.0/app.bin")
        .await
        .unwrap_err();
    assert!(matches!(old_style, StoreError::Conflict(_)));
    // A path outside the rule is free.
    assets
        .publish_with(&asset(repo.id, "snapshots/a", b"s"), &WriteOptions::new())
        .await
        .unwrap();
    assets
        .delete_by_path_checked(repo.id, "snapshots/a", &WriteOptions::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn an_admin_run_may_override_immutability_and_a_same_content_push_is_idempotent() {
    let env = env();
    let repo = hosted(&env.store, "override").await;
    let rule = path_rule(PolicyScope::Repository(repo.id), "**");
    env.store.immutability().create(&rule).await.unwrap();
    let assets = env.store.assets();
    let first = asset(repo.id, "f", b"same");
    assets
        .publish_with(&first, &WriteOptions::new())
        .await
        .unwrap();
    // Same SHA-256, new blob row: an idempotent re-push, not a change of content.
    let repush = asset(repo.id, "f", b"same");
    assert_eq!(repush.blob.checksums, first.blob.checksums);
    assets
        .publish_with(&repush, &WriteOptions::new())
        .await
        .unwrap();
    // Different content needs the override of an admin driven run.
    let changed = asset(repo.id, "f", b"different");
    let options = WriteOptions::new().with_override_immutable(true);
    assets.publish_with(&changed, &options).await.unwrap();
    let removed = assets
        .delete_by_path_checked(repo.id, "f", &options)
        .await
        .unwrap();
    assert_eq!(removed.map(|b| b.id), Some(changed.blob.id));
}

#[tokio::test]
async fn immutability_by_version_covers_the_assets_and_the_delete_of_the_component() {
    let env = env();
    let repo = hosted(&env.store, "versions").await;
    let mut rule = path_rule(PolicyScope::Repository(repo.id), "1.*");
    rule.item_kind = ItemKind::Version;
    env.store.immutability().create(&rule).await.unwrap();
    let mut new = NewComponent::new(repo.id, RepoFormat::Raw, "lib");
    new.version = Some("1.0".to_owned());
    let component = env.store.components().find_or_create(&new).await.unwrap();
    let mut first = asset(repo.id, "lib/1.0/lib.bin", b"v1");
    first.component_id = Some(component.id);
    env.store
        .assets()
        .publish_with(&first, &WriteOptions::new())
        .await
        .unwrap();

    let mut second = asset(repo.id, "lib/1.0/lib.bin", b"v2");
    second.component_id = Some(component.id);
    let refused = env
        .store
        .assets()
        .publish_with(&second, &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(
        violation(refused),
        PolicyViolation::Immutable { rule: rule.id }
    );
    let deleted = env
        .store
        .components()
        .delete_checked(component.id, &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(
        violation(deleted),
        PolicyViolation::Immutable { rule: rule.id }
    );
    let old_style = env
        .store
        .components()
        .delete(component.id)
        .await
        .unwrap_err();
    assert!(matches!(old_style, StoreError::Conflict(_)));
    // A version outside the rule is free, and the override removes a covered one.
    let mut other = NewComponent::new(repo.id, RepoFormat::Raw, "lib");
    other.version = Some("2.0".to_owned());
    let free = env.store.components().find_or_create(&other).await.unwrap();
    env.store
        .components()
        .delete_checked(free.id, &WriteOptions::new())
        .await
        .unwrap();
    env.store
        .components()
        .delete_checked(
            component.id,
            &WriteOptions::new().with_override_immutable(true),
        )
        .await
        .unwrap();
    assert!(
        env.store
            .components()
            .get(component.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn rules_apply_by_scope_selector_and_enabled_flag() {
    let env = env();
    let first = project_repo(&env, "app", "first").await;
    let other = project_repo(&env, "app", "second").await;
    let outsider = hosted(&env.store, "outside").await;

    // A project rule that names one repository by glob and one item glob.
    let mut rule = path_rule(PolicyScope::Project(first.0.id), "frozen/**");
    rule.repository = globs(&["fir*"]);
    env.store.immutability().create(&rule).await.unwrap();
    let covering = |repo: RepositoryId, item: &'static str| {
        let store = env.store.clone();
        async move {
            store
                .immutability()
                .covering_rule(repo, ItemKind::Path, item)
                .await
                .unwrap()
        }
    };
    assert_eq!(covering(first.1.id, "frozen/a").await, Some(rule.id));
    assert_eq!(covering(first.1.id, "open/a").await, None);
    assert_eq!(
        covering(other.1.id, "frozen/a").await,
        None,
        "repository glob"
    );
    assert_eq!(
        covering(outsider.id, "frozen/a").await,
        None,
        "other project"
    );
    assert_eq!(
        env.store
            .immutability()
            .covering_rule(first.1.id, ItemKind::Version, "frozen/a")
            .await
            .unwrap(),
        None,
        "item kind"
    );

    let mut disabled = rule.clone();
    disabled.enabled = false;
    env.store
        .immutability()
        .update(&disabled, rule.version)
        .await
        .unwrap();
    assert_eq!(
        covering(first.1.id, "frozen/a").await,
        None,
        "a disabled rule protects nothing"
    );
}

async fn project_repo(
    env: &common::Env,
    project_name: &str,
    repo_name: &str,
) -> (
    rustryx_core::project::Project,
    rustryx_core::model::Repository,
) {
    let existing = env
        .store
        .projects()
        .get_by_name(&rustryx_core::project::ProjectName::parse(project_name).unwrap())
        .await
        .unwrap();
    let project = match existing {
        Some(project) => project,
        None => project(&env.store, project_name).await,
    };
    let repo = hosted(&env.store, repo_name).await;
    env.store
        .projects()
        .attach_repository(project.id, repo.id)
        .await
        .unwrap();
    let repo = env.store.repos().get(repo.id).await.unwrap().unwrap();
    (project, repo)
}

// holds

#[tokio::test]
async fn must_not_change_or_delete_what_a_legal_hold_covers() {
    let env = env();
    let (project, repo) = project_repo(&env, "legal", "held").await;
    let mut new = NewComponent::new(repo.id, RepoFormat::Raw, "pkg");
    new.version = Some("1".to_owned());
    let component = env.store.components().find_or_create(&new).await.unwrap();
    let mut file = asset(repo.id, "pkg/file", b"v1");
    file.component_id = Some(component.id);
    env.store
        .assets()
        .publish_with(&file, &WriteOptions::new())
        .await
        .unwrap();

    for target in [
        HoldTarget::Component(component.id),
        HoldTarget::Repository(repo.id),
        HoldTarget::Project(project.id),
    ] {
        let hold = legal_hold(target);
        env.store.holds().create(&hold).await.unwrap();
        let mut update = asset(repo.id, "pkg/file", b"changed");
        update.component_id = Some(component.id);
        let overwrite = env
            .store
            .assets()
            .publish_with(&update, &WriteOptions::new())
            .await
            .unwrap_err();
        assert_eq!(
            violation(overwrite),
            PolicyViolation::Held { hold: hold.id },
            "{target:?}"
        );
        let delete = env
            .store
            .components()
            .delete_checked(component.id, &WriteOptions::new())
            .await
            .unwrap_err();
        assert_eq!(violation(delete), PolicyViolation::Held { hold: hold.id });
        // The override lifts immutability only: a legal hold stays.
        let overridden = env
            .store
            .assets()
            .delete_by_path_checked(
                repo.id,
                "pkg/file",
                &WriteOptions::new().with_override_immutable(true),
            )
            .await
            .unwrap_err();
        assert_eq!(
            violation(overridden),
            PolicyViolation::Held { hold: hold.id }
        );
        env.store
            .holds()
            .release(hold.id, None, env.clock_now())
            .await
            .unwrap();
    }
    // Everything released: the change goes through.
    let mut update = asset(repo.id, "pkg/file", b"changed");
    update.component_id = Some(component.id);
    env.store
        .assets()
        .publish_with(&update, &WriteOptions::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn only_an_active_legal_hold_stops_the_guard() {
    let env = env();
    let repo = hosted(&env.store, "kinds").await;
    let file = asset(repo.id, "f", b"v1");
    env.store
        .assets()
        .publish_with(&file, &WriteOptions::new())
        .await
        .unwrap();
    // A release hold stops lifecycle jobs, not the SQL guard.
    let mut release = legal_hold(HoldTarget::Repository(repo.id));
    release.kind = HoldKind::Release;
    env.store.holds().create(&release).await.unwrap();
    // A legal hold that expired a minute ago protects nothing.
    let mut expired = legal_hold(HoldTarget::Repository(repo.id));
    expired.expires_at = Some(env.clock_now() - time::Duration::minutes(1));
    env.store.holds().create(&expired).await.unwrap();
    env.store
        .assets()
        .publish_with(&asset(repo.id, "f", b"v2"), &WriteOptions::new())
        .await
        .unwrap();
    // A legal hold that expires later protects until then.
    let mut running = legal_hold(HoldTarget::Repository(repo.id));
    running.expires_at = Some(env.clock_now() + time::Duration::minutes(10));
    env.store.holds().create(&running).await.unwrap();
    let error = env
        .store
        .assets()
        .publish_with(&asset(repo.id, "f", b"v3"), &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(violation(error), PolicyViolation::Held { hold: running.id });
    env.clock.advance(Duration::from_mins(11));
    env.store
        .assets()
        .publish_with(&asset(repo.id, "f", b"v3"), &WriteOptions::new())
        .await
        .unwrap();
}

// quota

#[tokio::test]
async fn must_not_exceed_a_repository_quota_and_leave_nothing_behind() {
    let env = env();
    let repo = hosted(&env.store, "small").await;
    env.store
        .quotas()
        .set_limit(
            QuotaScope::Repository(repo.id),
            Some(10),
            80,
            None,
            &actor(),
        )
        .await
        .unwrap();
    let assets = env.store.assets();
    assets
        .publish_with(&asset(repo.id, "a", b"123456"), &WriteOptions::new())
        .await
        .unwrap();
    let over = asset(repo.id, "b", b"12345");
    let refused = assets
        .publish_with(
            &over,
            &WriteOptions::new().with_event(draft(EventKind::Push)),
        )
        .await
        .unwrap_err();
    match violation(refused) {
        PolicyViolation::QuotaExceeded(exceeded) => {
            assert_eq!(exceeded.scope, QuotaScope::Repository(repo.id));
            assert_eq!(
                (
                    exceeded.limit_bytes,
                    exceeded.used_bytes,
                    exceeded.requested_bytes
                ),
                (10, 6, 5)
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(env.store.blobs().get(over.blob.id).await.unwrap().is_none());
    assert!(assets.get_by_path(repo.id, "b").await.unwrap().is_none());
    assert!(
        env.store
            .outbox()
            .read_batch("c", 10)
            .await
            .unwrap()
            .events
            .is_empty()
    );
    // Replacing shrinks usage, so a bigger file fits where it replaces a bigger one.
    assets
        .publish_with(&asset(repo.id, "a", b"1234567890"), &WriteOptions::new())
        .await
        .unwrap();
    let usage = env
        .store
        .quotas()
        .get(QuotaScope::Repository(repo.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((usage.used_bytes, usage.object_count), (10, 1));
}

#[tokio::test]
async fn a_project_quota_counts_distinct_blobs_across_its_repositories() {
    let env = env();
    let (project, first) = project_repo(&env, "quota", "q-one").await;
    let (_, second) = project_repo(&env, "quota", "q-two").await;
    env.store
        .quotas()
        .set_limit(QuotaScope::Project(project.id), Some(8), 80, None, &actor())
        .await
        .unwrap();
    let assets = env.store.assets();
    let big = asset(first.id, "x", b"12345");
    assets
        .publish_with(&big, &WriteOptions::new())
        .await
        .unwrap();
    // Linking the same blob into a second repository of the project adds no bytes.
    let link = NewAssetLink::new(second.id, "x", "application/octet-stream", big.blob.id);
    assets.link(&link, &WriteOptions::new()).await.unwrap();
    let usage = env
        .store
        .quotas()
        .get(QuotaScope::Project(project.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(usage.used_bytes, 5);
    let error = assets
        .publish_with(&asset(second.id, "y", b"1234"), &WriteOptions::new())
        .await
        .unwrap_err();
    match violation(error) {
        PolicyViolation::QuotaExceeded(exceeded) => {
            assert_eq!(exceeded.scope, QuotaScope::Project(project.id));
        }
        other => panic!("{other:?}"),
    }
    let room = env.store.quotas().headroom(second.id).await.unwrap();
    assert_eq!(room.bytes, Some(3));
    assert_eq!(room.binding, Some(QuotaScope::Project(project.id)));
}

#[tokio::test]
async fn the_near_threshold_event_fires_once_per_climb() {
    let env = env();
    let repo = hosted(&env.store, "edge").await;
    env.store
        .quotas()
        .set_limit(
            QuotaScope::Repository(repo.id),
            Some(100),
            80,
            None,
            &actor(),
        )
        .await
        .unwrap();
    let assets = env.store.assets();
    let events = || async {
        let batch = env.store.outbox().read_batch("probe", 100).await.unwrap();
        batch
            .events
            .into_iter()
            .filter(|e| e.kind == EventKind::QuotaNearThreshold)
            .count()
    };
    assets
        .publish_with(&asset(repo.id, "a", &[0; 50]), &WriteOptions::new())
        .await
        .unwrap();
    assert_eq!(events().await, 0, "50 percent");
    assets
        .publish_with(&asset(repo.id, "b", &[0; 35]), &WriteOptions::new())
        .await
        .unwrap();
    assert_eq!(events().await, 1, "85 percent crosses 80");
    assets
        .publish_with(&asset(repo.id, "c", &[0; 5]), &WriteOptions::new())
        .await
        .unwrap();
    assert_eq!(events().await, 1, "no second event while above the edge");
    // Drop below warn percent minus 5 through deletes, then climb again.
    for path in ["b", "c"] {
        assets
            .delete_by_path_checked(repo.id, path, &WriteOptions::new())
            .await
            .unwrap();
    }
    assets
        .publish_with(&asset(repo.id, "d", &[0; 40]), &WriteOptions::new())
        .await
        .unwrap();
    assert_eq!(
        events().await,
        2,
        "the edge was re-armed by the delete path"
    );
}

// read only mode

#[tokio::test]
async fn must_not_write_assets_or_components_in_read_only_mode() {
    let env = env();
    let repo = hosted(&env.store, "ro-mode").await;
    let mut new = NewComponent::new(repo.id, RepoFormat::Raw, "pkg");
    new.version = Some("1".to_owned());
    let component = env.store.components().find_or_create(&new).await.unwrap();
    let mut file = asset(repo.id, "pkg/file", b"v1");
    file.component_id = Some(component.id);
    env.store
        .assets()
        .publish_with(&file, &WriteOptions::new())
        .await
        .unwrap();
    env.store.set_read_only(true, "backup").unwrap();

    let publish = env
        .store
        .assets()
        .publish_with(&asset(repo.id, "new", b"x"), &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(violation(publish), PolicyViolation::ReadOnly);
    let delete = env
        .store
        .assets()
        .delete_by_path_checked(repo.id, "pkg/file", &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(violation(delete), PolicyViolation::ReadOnly);
    let delete_component = env
        .store
        .components()
        .delete_checked(component.id, &WriteOptions::new())
        .await
        .unwrap_err();
    assert_eq!(violation(delete_component), PolicyViolation::ReadOnly);
    // Creating a component row is a write, finding one is not.
    assert_eq!(
        env.store
            .components()
            .find_or_create(&new)
            .await
            .unwrap()
            .id,
        component.id
    );
    let mut fresh = NewComponent::new(repo.id, RepoFormat::Raw, "other");
    fresh.version = Some("2".to_owned());
    let created = env
        .store
        .components()
        .find_or_create(&fresh)
        .await
        .unwrap_err();
    assert!(matches!(created, StoreError::Conflict(_)));
    // The cascade of a repository delete is stopped as well.
    let cascade = env.store.repos().delete(repo.id).await.unwrap_err();
    assert!(matches!(cascade, StoreError::Conflict(_)));
    // Downloads are not writes: counters and stamps keep moving.
    let stored = env
        .store
        .assets()
        .get_by_path(repo.id, "pkg/file")
        .await
        .unwrap()
        .unwrap();
    env.store
        .assets()
        .record_downloads(&[DownloadTick {
            asset_id: stored.id,
            at: env.clock_now(),
            count: 1,
        }])
        .await
        .unwrap();

    env.store.set_read_only(false, "").unwrap();
    env.store
        .assets()
        .publish_with(&asset(repo.id, "new", b"x"), &WriteOptions::new())
        .await
        .unwrap();
}

// link

#[tokio::test]
async fn link_publishes_by_reference_with_the_same_guards() {
    let env = env();
    let source = hosted(&env.store, "source").await;
    let target = hosted(&env.store, "target").await;
    let original = asset(source.id, "lib.bin", b"payload");
    env.store
        .assets()
        .publish_with(&original, &WriteOptions::new())
        .await
        .unwrap();

    let link = NewAssetLink::new(target.id, "copy.bin", "application/x-tar", original.blob.id);
    let outcome = env
        .store
        .assets()
        .link(
            &link,
            &WriteOptions::new().with_event(draft(EventKind::Push)),
        )
        .await
        .unwrap();
    assert!(outcome.created);
    assert_eq!(outcome.asset.blob_id, original.blob.id);
    assert_eq!(outcome.asset.size_bytes, 7);
    assert_eq!(outcome.asset.content_type, "application/x-tar");
    assert_eq!(
        env.store
            .outbox()
            .read_batch("c", 10)
            .await
            .unwrap()
            .events
            .len(),
        1
    );
    // Replacing a link whose blob the source still references hands nothing back for GC.
    let replaced = env
        .store
        .assets()
        .publish_with(
            &asset(target.id, "copy.bin", b"other"),
            &WriteOptions::new(),
        )
        .await
        .unwrap();
    assert!(
        replaced.replaced.is_none(),
        "the source asset still references the blob"
    );

    let missing = NewAssetLink::new(target.id, "x", "t", rustryx_core::ids::BlobId::generate());
    let error = env
        .store
        .assets()
        .link(&missing, &WriteOptions::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        MutationError::Store(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn must_not_link_across_blob_stores_unless_a_group_serves_the_member() {
    let env = env();
    let source = hosted(&env.store, "plain").await;
    let original = asset(source.id, "lib.bin", b"payload");
    env.store
        .assets()
        .publish_with(&original, &WriteOptions::new())
        .await
        .unwrap();

    let other_store = rustryx_core::model::BlobStoreDef::new(
        rustryx_core::ids::BlobStoreId::generate(),
        "elsewhere",
        rustryx_core::model::BlobStoreKind::Local,
        serde_json::json!({}),
        common::T0,
    );
    env.store.blob_stores().create(&other_store).await.unwrap();
    let mut foreign = repository("foreign", RepoFormat::Raw, RepoType::Hosted);
    foreign.blob_store_id = other_store.id;
    env.store.repos().create(&foreign).await.unwrap();
    let link = NewAssetLink::new(foreign.id, "copy", "t", original.blob.id);
    let refused = env
        .store
        .assets()
        .link(&link, &WriteOptions::new())
        .await
        .unwrap_err();
    assert!(matches!(
        refused,
        MutationError::Store(StoreError::Conflict(_))
    ));

    // A group over the blob's store serves it, so a repository on the group links.
    let group = rustryx_core::model::BlobStoreDef::new(
        rustryx_core::ids::BlobStoreId::generate(),
        "pool",
        rustryx_core::model::BlobStoreKind::Group,
        serde_json::json!({}),
        common::T0,
    );
    env.store
        .store_members()
        .convert_to_group(rustryx_testkit::fixtures::BLOB_STORE_ID, &group)
        .await
        .unwrap();
    let pooled = env.store.repos().get(source.id).await.unwrap().unwrap();
    assert_eq!(pooled.blob_store_id, group.id);
    let grouped = NewAssetLink::new(source.id, "copy", "t", original.blob.id);
    env.store
        .assets()
        .link(&grouped, &WriteOptions::new())
        .await
        .unwrap();
}

// events

#[tokio::test]
async fn events_are_written_in_the_step_that_changes_the_state() {
    let env = env();
    let repo = hosted(&env.store, "events").await;
    let options = WriteOptions::new()
        .with_event(draft(EventKind::Push))
        .with_event(draft(EventKind::Delete));
    env.store
        .assets()
        .publish_with(&asset(repo.id, "a", b"1"), &options)
        .await
        .unwrap();
    let batch = env.store.outbox().read_batch("c", 10).await.unwrap();
    let kinds: Vec<EventKind> = batch.events.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [EventKind::Push, EventKind::Delete]);

    // An oversized event is `Invalid` and nothing of the publish is written.
    let big = EventDraft::new(
        EventKind::Push,
        actor(),
        serde_json::json!({ "blob": "x".repeat(20_000) }),
    );
    let oversized = asset(repo.id, "b", b"2");
    let error = env
        .store
        .assets()
        .publish_with(&oversized, &WriteOptions::new().with_event(big))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        MutationError::Store(StoreError::Invalid(_))
    ));
    assert!(
        env.store
            .blobs()
            .get(oversized.blob.id)
            .await
            .unwrap()
            .is_none()
    );
    let removal = env
        .store
        .assets()
        .delete_by_path_checked(
            repo.id,
            "a",
            &WriteOptions::new().with_event(draft(EventKind::Delete)),
        )
        .await
        .unwrap();
    assert!(removal.is_some());
    assert_eq!(
        env.store
            .outbox()
            .read_batch("other", 10)
            .await
            .unwrap()
            .events
            .len(),
        3
    );
}

// delete semantics

#[tokio::test]
async fn delete_by_path_checked_returns_only_a_blob_that_lost_its_last_reference() {
    let env = env();
    let source = hosted(&env.store, "owner").await;
    let target = hosted(&env.store, "sharer").await;
    let original = asset(source.id, "x", b"shared");
    env.store
        .assets()
        .publish_with(&original, &WriteOptions::new())
        .await
        .unwrap();
    env.store
        .assets()
        .link(
            &NewAssetLink::new(target.id, "y", "t", original.blob.id),
            &WriteOptions::new(),
        )
        .await
        .unwrap();
    let first = env
        .store
        .assets()
        .delete_by_path_checked(source.id, "x", &WriteOptions::new())
        .await
        .unwrap();
    assert!(first.is_none(), "the link still references the blob");
    let last = env
        .store
        .assets()
        .delete_by_path_checked(target.id, "y", &WriteOptions::new())
        .await
        .unwrap();
    assert_eq!(last.map(|b| b.id), Some(original.blob.id));
    let missing = env
        .store
        .assets()
        .delete_by_path_checked(target.id, "y", &WriteOptions::new())
        .await
        .unwrap();
    assert!(missing.is_none(), "an absent path is None, not an error");
    let error = env
        .store
        .components()
        .delete_checked(ComponentId::generate(), &WriteOptions::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        MutationError::Store(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn must_not_delete_a_repository_whose_content_is_frozen() {
    let env = env();
    let repo = hosted(&env.store, "doomed").await;
    env.store
        .assets()
        .publish_with(&asset(repo.id, "keep/a", b"1"), &WriteOptions::new())
        .await
        .unwrap();
    let rule = path_rule(PolicyScope::Repository(repo.id), "keep/**");
    env.store.immutability().create(&rule).await.unwrap();
    let error = env.store.repos().delete(repo.id).await.unwrap_err();
    assert!(matches!(error, StoreError::Conflict(_)), "{error:?}");
    assert!(env.store.repos().get(repo.id).await.unwrap().is_some());
    env.store.immutability().delete(rule.id).await.unwrap();
    env.store.repos().delete(repo.id).await.unwrap();
    // The rule went with the repository it was scoped to.
    assert!(
        env.store
            .immutability()
            .get(rule.id)
            .await
            .unwrap()
            .is_none()
    );
}

// downloads

#[tokio::test]
async fn record_downloads_moves_stamps_and_daily_counters_and_ignores_unknown_assets() {
    let env = env();
    let repo = hosted(&env.store, "stats").await;
    let mut new = NewComponent::new(repo.id, RepoFormat::Raw, "pkg");
    new.version = Some("1".to_owned());
    let component = env.store.components().find_or_create(&new).await.unwrap();
    let mut file = asset(repo.id, "pkg/file", b"v1");
    file.component_id = Some(component.id);
    let published = env
        .store
        .assets()
        .publish_with(&file, &WriteOptions::new())
        .await
        .unwrap()
        .asset;
    let today = env.clock_now();
    let yesterday = today - time::Duration::days(1);
    let ghost = rustryx_core::ids::AssetId::generate();
    env.store
        .assets()
        .record_downloads(&[
            DownloadTick {
                asset_id: published.id,
                at: yesterday,
                count: 2,
            },
            DownloadTick {
                asset_id: published.id,
                at: today,
                count: 3,
            },
            DownloadTick {
                asset_id: published.id,
                at: yesterday,
                count: 1,
            },
            DownloadTick {
                asset_id: ghost,
                at: today,
                count: 9,
            },
        ])
        .await
        .unwrap();
    let stored = env.store.assets().get(published.id).await.unwrap().unwrap();
    assert_eq!(stored.last_downloaded_at, Some(today), "forward only");
    let component = env
        .store
        .components()
        .get(component.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(component.last_downloaded_at, Some(today));
    let series = env
        .store
        .download_stats()
        .daily(published.id, 3)
        .await
        .unwrap();
    let counts: Vec<u64> = series.iter().map(|(_, n)| *n).collect();
    assert_eq!(counts, [0, 3, 3], "oldest first, gaps filled with zero");
    assert_eq!(series[2].0, today.date());
    let nothing = env.store.download_stats().daily(ghost, 0).await.unwrap();
    assert!(nothing.is_empty());
    let capped = env
        .store
        .download_stats()
        .daily(published.id, 1000)
        .await
        .unwrap();
    assert_eq!(capped.len(), 90);
}

#[tokio::test]
async fn touch_fresh_moves_updated_at_forward_only() {
    let env = env();
    let repo = hosted(&env.store, "fresh").await;
    let published = env
        .store
        .assets()
        .publish_with(&asset(repo.id, "f", b"v1"), &WriteOptions::new())
        .await
        .unwrap()
        .asset;
    let later = published.updated_at + time::Duration::minutes(5);
    env.store
        .assets()
        .touch_fresh(published.id, later)
        .await
        .unwrap();
    env.store
        .assets()
        .touch_fresh(published.id, published.updated_at)
        .await
        .unwrap();
    let stored = env.store.assets().get(published.id).await.unwrap().unwrap();
    assert_eq!(stored.updated_at, later);
    assert_eq!(stored.blob_id, published.blob_id, "the blob is not touched");
    let error = env
        .store
        .assets()
        .touch_fresh(rustryx_core::ids::AssetId::generate(), later)
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::NotFound(_)));
}

// garbage collection

#[tokio::test]
async fn gc_measures_age_from_the_moment_the_last_reference_went_away() {
    let env = env();
    let repo = hosted(&env.store, "gc").await;
    let mut old = asset(repo.id, "f", b"v1");
    old.blob.created_at = datetime!(2020-01-01 00:00:00 UTC);
    env.store
        .assets()
        .publish_with(&old, &WriteOptions::new())
        .await
        .unwrap();
    let cutoff = env.clock_now() - time::Duration::hours(1);
    let blobs = env.store.blobs();
    // Referenced: never a candidate, however old.
    assert!(
        blobs
            .list_unreferenced(cutoff, 10)
            .await
            .unwrap()
            .is_empty()
    );
    // Replaced just now: old by creation, young by orphaning, so GC must wait.
    env.store
        .assets()
        .publish_with(&asset(repo.id, "f", b"v2"), &WriteOptions::new())
        .await
        .unwrap();
    assert!(
        blobs
            .list_unreferenced(cutoff, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !blobs
            .delete_if_unreferenced(old.blob.id, cutoff)
            .await
            .unwrap()
    );
    env.clock.advance(Duration::from_hours(2));
    let later_cutoff = env.clock_now() - time::Duration::hours(1);
    let due = blobs.list_unreferenced(later_cutoff, 10).await.unwrap();
    assert_eq!(due.iter().map(|b| b.id).collect::<Vec<_>>(), [old.blob.id]);
    let in_store = blobs
        .list_unreferenced_in(rustryx_testkit::fixtures::BLOB_STORE_ID, later_cutoff, 10)
        .await
        .unwrap();
    assert_eq!(in_store.len(), 1);
    let elsewhere = blobs
        .list_unreferenced_in(rustryx_core::ids::BlobStoreId::generate(), later_cutoff, 10)
        .await
        .unwrap();
    assert!(elsewhere.is_empty());
    // A link made after the mark wins: the blob is referenced again and kept.
    env.store
        .assets()
        .link(
            &NewAssetLink::new(repo.id, "revived", "t", old.blob.id),
            &WriteOptions::new(),
        )
        .await
        .unwrap();
    assert!(
        !blobs
            .delete_if_unreferenced(old.blob.id, later_cutoff)
            .await
            .unwrap()
    );
    assert!(blobs.get(old.blob.id).await.unwrap().is_some());
    // Unreferenced and old enough: the row goes, and a second call finds nothing to do.
    env.store
        .assets()
        .delete_by_path_checked(repo.id, "revived", &WriteOptions::new())
        .await
        .unwrap();
    env.clock.advance(Duration::from_hours(2));
    let final_cutoff = env.clock_now() - time::Duration::hours(1);
    assert!(
        blobs
            .delete_if_unreferenced(old.blob.id, final_cutoff)
            .await
            .unwrap()
    );
    assert!(
        !blobs
            .delete_if_unreferenced(old.blob.id, final_cutoff)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn blob_rows_can_be_inserted_moved_and_listed_by_store() {
    let env = env();
    let blobs = env.store.blobs();
    let first = blob_record("k1", b"a");
    let second = blob_record("k2", b"b");
    blobs.insert(&first).await.unwrap();
    blobs.insert(&second).await.unwrap();
    let mut same_key = blob_record("k1", b"c");
    same_key.id = rustryx_core::ids::BlobId::generate();
    assert!(matches!(
        blobs.insert(&same_key).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        blobs.insert(&first).await,
        Err(StoreError::Conflict(_))
    ));
    let listed = blobs
        .list_page_by_store(rustryx_testkit::fixtures::BLOB_STORE_ID, None, 1, false)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let rest = blobs
        .list_page_by_store(
            rustryx_testkit::fixtures::BLOB_STORE_ID,
            Some(listed[0].id),
            10,
            false,
        )
        .await
        .unwrap();
    assert_eq!(rest.len(), 1);
    assert_ne!(rest[0].id, listed[0].id);
    let referenced = blobs
        .list_page_by_store(rustryx_testkit::fixtures::BLOB_STORE_ID, None, 10, true)
        .await
        .unwrap();
    assert!(referenced.is_empty(), "no asset references these rows");

    let target = rustryx_core::model::BlobStoreDef::new(
        rustryx_core::ids::BlobStoreId::generate(),
        "second",
        rustryx_core::model::BlobStoreKind::Local,
        serde_json::json!({}),
        common::T0,
    );
    env.store.blob_stores().create(&target).await.unwrap();
    let from = rustryx_testkit::fixtures::BLOB_STORE_ID;
    assert!(blobs.move_blob(first.id, from, target.id).await.unwrap());
    assert!(
        !blobs.move_blob(first.id, from, target.id).await.unwrap(),
        "already moved"
    );
    assert!(
        !blobs
            .move_blob(rustryx_core::ids::BlobId::generate(), from, target.id)
            .await
            .unwrap()
    );
    let missing_target = blobs
        .move_blob(second.id, from, rustryx_core::ids::BlobStoreId::generate())
        .await
        .unwrap_err();
    assert!(matches!(missing_target, StoreError::Conflict(_)));
    assert_eq!(
        blobs.get(first.id).await.unwrap().unwrap().blob_store_id,
        target.id
    );
}

#[tokio::test]
async fn public_project_visibility_flows_into_attached_repositories() {
    let env = env();
    let project = public_project(&env.store, "open").await;
    let repo = hosted(&env.store, "pub-repo").await;
    assert!(!repo.allow_anonymous);
    env.store
        .projects()
        .attach_repository(project.id, repo.id)
        .await
        .unwrap();
    let attached = env.store.repos().get(repo.id).await.unwrap().unwrap();
    assert!(
        attached.allow_anonymous,
        "a repository in a public project is public"
    );
    assert_eq!(attached.project.as_ref().map(|p| p.id), Some(project.id));
}
