//! What a delete takes along: the rows that hang off a repository, a component or a project
//! through `ON DELETE CASCADE`, and what stays.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    reason = "each test is one scenario read top to bottom"
)]

mod common;

use common::{Ports, T0, asset, env, hosted, page, project};
use rustryx_core::audit::ActorRef;
use rustryx_core::events::EventKind;
use rustryx_core::ids::{
    CveAllowlistEntryId, ImmutabilityRuleId, LifecycleHoldId, RetentionPolicyId, ScanReportId,
    SecurityPolicyId, SubscriptionId, TrustPolicyId, WebhookId,
};
use rustryx_core::lifecycle::{
    HoldKind, HoldTarget, ImmutabilityRule, ItemKind, LifecycleHold, NameSelector, PolicyScope,
    RetentionPolicy, RetentionRule, RetentionTemplate,
};
use rustryx_core::model::{NewComponent, RepoFormat, WriteOptions};
use rustryx_core::notification::{NotificationSubscription, Recipient};
use rustryx_core::promotion::PromotionTarget;
use rustryx_core::quota::QuotaScope;
use rustryx_core::scan::{
    AllowlistScope, CveAllowlistEntry, ScanKind, ScanPolicy, ScanReport, ScanSummary, ScanTrigger,
    ScannerKind, TrustPolicy,
};
use rustryx_core::secret::Secret;
use rustryx_core::webhook::{NotifyScope, Webhook};

fn retention(scope: PolicyScope) -> RetentionPolicy {
    let rule = RetentionRule {
        template: RetentionTemplate::KeepLatestVersions { count: 1 },
        repository: NameSelector::everything(),
        item: NameSelector::everything(),
        include_untagged: false,
    };
    RetentionPolicy::new(
        RetentionPolicyId::generate(),
        scope,
        "keep one",
        vec![rule],
        T0,
    )
}

fn hook(scope: NotifyScope) -> Webhook {
    Webhook::new(
        WebhookId::generate(),
        scope,
        "hook",
        "https://hooks.example.test/x",
        Secret::new("signing-secret-0123456789"),
        vec![EventKind::Push],
        T0,
    )
}

fn subscription(scope: NotifyScope) -> NotificationSubscription {
    NotificationSubscription::new(
        SubscriptionId::generate(),
        scope,
        vec![EventKind::Push],
        vec![Recipient::TokenOwner],
    )
}

#[tokio::test]
async fn deleting_a_repository_removes_what_is_scoped_to_it_and_leaves_the_rest() {
    let env = env();
    let doomed = hosted(&env.store, "doomed").await;
    let survivor = hosted(&env.store, "survivor").await;
    let mut new = NewComponent::new(doomed.id, RepoFormat::Raw, "pkg");
    new.version = Some("1".to_owned());
    let component = env.store.components().find_or_create(&new).await.unwrap();
    let mut file = asset(doomed.id, "pkg/file", b"payload");
    file.component_id = Some(component.id);
    env.store
        .assets()
        .publish_with(&file, &WriteOptions::new())
        .await
        .unwrap();

    let scope = PolicyScope::Repository(doomed.id);
    env.store
        .retention()
        .upsert(&retention(scope), None)
        .await
        .unwrap();
    env.store
        .security_policies()
        .upsert_scan_policy(&ScanPolicy::new(SecurityPolicyId::generate(), scope), None)
        .await
        .unwrap();
    env.store
        .security_policies()
        .upsert_trust_policy(&TrustPolicy::new(TrustPolicyId::generate(), scope), None)
        .await
        .unwrap();
    let doomed_hook = hook(NotifyScope::Repository(doomed.id));
    env.store.webhooks().create(&doomed_hook).await.unwrap();
    env.store
        .notifications()
        .upsert(&subscription(NotifyScope::Repository(doomed.id)), None)
        .await
        .unwrap();
    env.store
        .promotions()
        .set_target(&PromotionTarget::new(doomed.id), None)
        .await
        .unwrap();
    let on_repo = LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Release,
        HoldTarget::Repository(doomed.id),
        "standing hold",
        T0,
    );
    env.store.holds().create(&on_repo).await.unwrap();
    let rule = ImmutabilityRule::new(
        ImmutabilityRuleId::generate(),
        scope,
        "frozen",
        ItemKind::Path,
        NameSelector::matching(vec!["never/**".to_owned()]).unwrap(),
        T0,
    );
    env.store.immutability().create(&rule).await.unwrap();
    env.store
        .quotas()
        .set_limit(
            QuotaScope::Repository(doomed.id),
            Some(1000),
            80,
            None,
            &ActorRef::system("test"),
        )
        .await
        .unwrap();
    let kept_hook = hook(NotifyScope::Repository(survivor.id));
    env.store.webhooks().create(&kept_hook).await.unwrap();
    let report = ScanReport::new(
        ScanReportId::generate(),
        component.id,
        ScannerKind::Trivy,
        ScanKind::Vulnerability,
        ScanTrigger::Manual,
        T0,
    );
    env.store.scans().start(&report).await.unwrap();
    env.store
        .scans()
        .complete(report.id, &[], &ScanSummary::default(), "v", "d", T0, &[])
        .await
        .unwrap();
    let entry = CveAllowlistEntry::new(
        CveAllowlistEntryId::generate(),
        AllowlistScope::Component(component.id),
        "CVE-1",
        "accepted",
        T0,
    );
    env.store.allowlist().create(&entry).await.unwrap();
    assert!(
        env.store
            .scans()
            .state(component.id)
            .await
            .unwrap()
            .is_some()
    );

    env.store.repos().delete(doomed.id).await.unwrap();

    assert!(
        env.store
            .components()
            .get(component.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .assets()
            .get_by_path(doomed.id, "pkg/file")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .retention()
            .get_for_scope(scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .security_policies()
            .get_scan_policy(scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .security_policies()
            .get_trust_policy(scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .webhooks()
            .get(doomed_hook.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .notifications()
            .list(Some(NotifyScope::Repository(doomed.id)))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        env.store
            .promotions()
            .get_target(doomed.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(env.store.holds().get(on_repo.id).await.unwrap().is_none());
    assert!(
        env.store
            .immutability()
            .get(rule.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .quotas()
            .get(QuotaScope::Repository(doomed.id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .scans()
            .state(component.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .scans()
            .get_report(report.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .allowlist()
            .list(AllowlistScope::Component(component.id), true, &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        env.store
            .webhooks()
            .get(kept_hook.id)
            .await
            .unwrap()
            .is_some(),
        "another repository's webhook stays"
    );
    // The blob row stays for garbage collection.
    assert!(env.store.blobs().get(file.blob.id).await.unwrap().is_some());
}

#[tokio::test]
async fn deleting_a_project_removes_its_policies_exceptions_and_notifiers() {
    let env = env();
    let created = project(&env.store, "tidy").await;
    let scope = PolicyScope::Project(created.id);
    env.store
        .retention()
        .upsert(&retention(scope), None)
        .await
        .unwrap();
    env.store
        .security_policies()
        .upsert_scan_policy(&ScanPolicy::new(SecurityPolicyId::generate(), scope), None)
        .await
        .unwrap();
    env.store
        .security_policies()
        .upsert_trust_policy(&TrustPolicy::new(TrustPolicyId::generate(), scope), None)
        .await
        .unwrap();
    let doomed_hook = hook(NotifyScope::Project(created.id));
    env.store.webhooks().create(&doomed_hook).await.unwrap();
    env.store
        .deliveries()
        .create_for_events(
            &[(
                doomed_hook.id,
                rustryx_core::ids::EventId::generate(),
                EventKind::Push,
            )],
            None,
        )
        .await
        .unwrap();
    env.store
        .notifications()
        .upsert(&subscription(NotifyScope::Project(created.id)), None)
        .await
        .unwrap();
    let entry = CveAllowlistEntry::new(
        CveAllowlistEntryId::generate(),
        AllowlistScope::Project(created.id),
        "CVE-2",
        "accepted",
        T0,
    );
    env.store.allowlist().create(&entry).await.unwrap();
    let hold = LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Legal,
        HoldTarget::Project(created.id),
        "case 12",
        T0,
    );
    env.store.holds().create(&hold).await.unwrap();
    env.store
        .quotas()
        .set_limit(
            QuotaScope::Project(created.id),
            Some(10),
            80,
            None,
            &ActorRef::system("test"),
        )
        .await
        .unwrap();
    assert_eq!(
        env.store
            .projects()
            .dependents(created.id)
            .await
            .unwrap()
            .webhooks,
        1
    );

    env.store.projects().delete(created.id).await.unwrap();

    assert!(
        env.store
            .retention()
            .get_for_scope(scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .security_policies()
            .get_scan_policy(scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .security_policies()
            .get_trust_policy(scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        env.store
            .webhooks()
            .get(doomed_hook.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(env.store.deliveries().pending_count().await.unwrap(), 0);
    assert!(
        env.store
            .notifications()
            .list(Some(NotifyScope::Project(created.id)))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        env.store
            .allowlist()
            .list(AllowlistScope::Project(created.id), true, &page(10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(env.store.holds().get(hold.id).await.unwrap().is_none());
    assert!(
        env.store
            .quotas()
            .get(QuotaScope::Project(created.id))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn deleting_a_component_removes_its_hold_and_assets_but_not_its_blob_rows() {
    let env = env();
    let repo = hosted(&env.store, "components").await;
    let mut new = NewComponent::new(repo.id, RepoFormat::Raw, "pkg");
    new.version = Some("1".to_owned());
    let component = env.store.components().find_or_create(&new).await.unwrap();
    let mut file = asset(repo.id, "pkg/file", b"x");
    file.component_id = Some(component.id);
    env.store
        .assets()
        .publish_with(&file, &WriteOptions::new())
        .await
        .unwrap();
    // A release hold stops lifecycle jobs, not the SQL guard, and goes with the component.
    let hold = LifecycleHold::new(
        LifecycleHoldId::generate(),
        HoldKind::Release,
        HoldTarget::Component(component.id),
        "release",
        T0,
    );
    env.store.holds().create(&hold).await.unwrap();
    env.store.components().delete(component.id).await.unwrap();
    assert!(env.store.holds().get(hold.id).await.unwrap().is_none());
    assert!(env.store.blobs().get(file.blob.id).await.unwrap().is_some());
    assert!(
        env.store
            .assets()
            .list_by_component(component.id)
            .await
            .unwrap()
            .is_empty()
    );
}
