//! Every metadata port is served by one `MemStore` as a shared trait object, and
//! `set_unavailable` reaches every one of them: a call on a switched off store fails with
//! `StoreError::Unavailable` and never answers from stale tables.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
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

use std::sync::Arc;
use std::time::Duration;

use common::{Ports, T0, page};
use rustryx_core::audit::ActorRef;
use rustryx_core::error::{MutationError, StoreError};
use rustryx_core::events::{CursorAdvance, OutboxCursor};
use rustryx_core::ids::{
    AssetId, BlobId, BlobStoreId, CleanupPolicyId, ComponentId, ExportId, GcRunId,
    ImmutabilityRuleId, LabelId, LifecycleHoldId, LifecycleRunId, ProjectId, ProjectMemberId,
    PromotionId, RegistryId, ReplicationExecutionId, ReplicationRuleId, ReplicationTaskId,
    RepositoryId, RetentionPolicyId, RobotTokenId, RoutingRuleId, ScanReportId, ScannerId,
    SubscriptionId, TaskDefinitionId, TaskRunId, TrustMaterialId, TrustedCertificateId, UserId,
    VerifyRunId, WebhookId,
};
use rustryx_core::label::LabelScope;
use rustryx_core::lifecycle::{ItemKind, LifecycleCounts, PolicyScope};
use rustryx_core::model::{NewComponent, RepoFormat, WriteOptions};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{
    AuditQuery, ComponentFilter, FindingFilter, HubFilter, LifecycleScope, NodeRegistryRepo,
    PolicyRef, ProjectVisibilityFilter, RobotFilter, SecretView,
};
use rustryx_core::project::ProjectRole;
use rustryx_core::quota::QuotaScope;
use rustryx_core::scan::{AllowlistMode, ScannerKind};
use rustryx_core::tasks::TaskRunStatus;
use rustryx_testkit::MemStore;
use uuid::Uuid;

macro_rules! unavailable {
    ($call:expr) => {{
        let outcome = $call.await;
        assert!(
            matches!(outcome, Err(StoreError::Unavailable(_))),
            "{}: {outcome:?}",
            stringify!($call)
        );
    }};
}

macro_rules! unavailable_mutation {
    ($call:expr) => {{
        let outcome = $call.await;
        assert!(
            matches!(
                outcome,
                Err(MutationError::Store(StoreError::Unavailable(_)))
            ),
            "{}: {outcome:?}",
            stringify!($call)
        );
    }};
}

fn actor() -> ActorRef {
    ActorRef::system("test")
}

#[tokio::test]
async fn one_store_serves_every_new_port_as_a_trait_object() {
    // Instantiating every accessor of `Ports` proves the store implements the port and that
    // the port is dyn compatible, the way the composition root holds it.
    let store = MemStore::new();
    let held: Vec<Arc<dyn std::any::Any + Send + Sync>> = vec![
        Arc::new(store.repos()),
        Arc::new(store.blob_stores()),
        Arc::new(store.blobs()),
        Arc::new(store.components()),
        Arc::new(store.assets()),
        Arc::new(store.audit_log()),
        Arc::new(store.revoked()),
        Arc::new(store.jobs()),
        Arc::new(store.routing_rules()),
        Arc::new(store.projects()),
        Arc::new(store.members()),
        Arc::new(store.access()),
        Arc::new(store.labels()),
        Arc::new(store.robots()),
        Arc::new(store.quotas()),
        Arc::new(store.immutability()),
        Arc::new(store.holds()),
        Arc::new(store.cleanup_policies()),
        Arc::new(store.retention()),
        Arc::new(store.lifecycle()),
        Arc::new(store.lifecycle_runs()),
        Arc::new(store.gc_runs()),
        Arc::new(store.download_stats()),
        Arc::new(store.scanners()),
        Arc::new(store.scans()),
        Arc::new(store.allowlist()),
        Arc::new(store.security_policies()),
        Arc::new(store.trust()),
        Arc::new(store.hub()),
        Arc::new(store.exports()),
        Arc::new(store.datasets()),
        Arc::new(store.registries()),
        Arc::new(store.replication()),
        Arc::new(store.webhooks()),
        Arc::new(store.deliveries()),
        Arc::new(store.notifications()),
        Arc::new(store.promotions()),
        Arc::new(store.store_members()),
        Arc::new(store.blob_moves()),
        Arc::new(store.verify_runs()),
        Arc::new(store.store_stats()),
        Arc::new(store.outbox()),
        Arc::new(store.task_definitions()),
        Arc::new(store.task_runs()),
        Arc::new(store.nodes()),
        Arc::new(store.config()),
        Arc::new(store.certificates()),
    ];
    assert_eq!(held.len(), 47);
}

#[tokio::test]
async fn a_switched_off_store_fails_the_first_group_of_ports() {
    let store = MemStore::new();
    store.set_unavailable(true);
    let (repo, blob_store, blob, component, asset) = (
        RepositoryId::generate(),
        BlobStoreId::generate(),
        BlobId::generate(),
        ComponentId::generate(),
        AssetId::generate(),
    );
    let name = RepoName::parse("some-repo").unwrap();
    let options = WriteOptions::new();
    let at = T0;
    unavailable!(store.repos().get(repo));
    unavailable!(store.repos().get_by_name(&name));
    unavailable!(store.repos().list());
    unavailable!(store.repos().delete(repo));
    unavailable!(store.blob_stores().get(blob_store));
    unavailable!(store.blob_stores().list());
    unavailable!(store.blobs().get(blob));
    unavailable!(store.blobs().list_unreferenced(at, 1));
    unavailable!(store.blobs().list_unreferenced_in(blob_store, at, 1));
    unavailable!(store.blobs().delete_if_unreferenced(blob, at));
    unavailable!(store.blobs().move_blob(blob, blob_store, blob_store));
    unavailable!(store.blobs().list_page_by_store(blob_store, None, 1, false));
    unavailable!(store.components().get(component));
    unavailable!(
        store
            .components()
            .list(&ComponentFilter::default(), &page(1))
    );
    unavailable!(
        store
            .components()
            .find_or_create(&NewComponent::new(repo, RepoFormat::Raw, "x"))
    );
    unavailable_mutation!(store.components().delete_checked(component, &options));
    unavailable!(store.assets().get(asset));
    unavailable!(store.assets().get_by_path(repo, "x"));
    unavailable!(store.assets().touch_fresh(asset, at));
    unavailable!(store.assets().record_downloads(&[]));
    unavailable_mutation!(store.assets().delete_by_path_checked(repo, "x", &options));
    unavailable!(
        store
            .users()
            .set_password_hash_if(UserId::generate(), "a", "b")
    );
    unavailable!(store.tokens().list_expiring(Duration::from_secs(1), at, 1));
    unavailable!(
        store
            .tokens()
            .mark_expiry_notified(rustryx_core::ids::ApiTokenId::generate(), at)
    );
    unavailable!(store.revoked().is_revoked(Uuid::now_v7()));
    unavailable!(store.revoked().purge_expired(at));
    unavailable!(store.jobs().depth("q"));
    unavailable!(store.jobs().claim("q", "o", Duration::from_secs(1)));
    unavailable!(store.jobs().enqueue("q", None, &serde_json::json!({})));
    unavailable!(store.routing_rules().list());
    unavailable!(store.routing_rules().get(RoutingRuleId::generate()));
    unavailable!(store.audit_log().query(&AuditQuery::new(), &page(1)));
}

#[tokio::test]
async fn a_switched_off_store_fails_projects_labels_robots_and_policies() {
    let store = MemStore::new();
    store.set_unavailable(true);
    let project = ProjectId::generate();
    let user = UserId::generate();
    let at = T0;
    unavailable!(store.projects().get(project));
    unavailable!(
        store
            .projects()
            .list_visible(&ProjectVisibilityFilter::All, None, &page(1))
    );
    unavailable!(store.projects().dependents(project));
    unavailable!(store.projects().detach_repository(RepositoryId::generate()));
    unavailable!(store.members().list(project, &page(1)));
    unavailable!(store.members().get(ProjectMemberId::generate()));
    unavailable!(store.members().remove(ProjectMemberId::generate(), false));
    unavailable!(
        store
            .members()
            .set_role(ProjectMemberId::generate(), ProjectRole::Guest, false)
    );
    unavailable!(store.access().memberships_of_user(user));
    unavailable!(
        store
            .access()
            .group_mappings(rustryx_core::identity::IdpProvider::Oidc, 1)
    );
    unavailable!(store.labels().get(LabelId::generate()));
    unavailable!(store.labels().list(LabelScope::System, &page(1)));
    unavailable!(store.labels().labels_of(ComponentId::generate()));
    unavailable!(store.labels().usage(LabelId::generate()));
    unavailable!(store.labels().delete(LabelId::generate()));
    unavailable!(store.robots().get(RobotTokenId::generate()));
    unavailable!(store.robots().find_by_hash("h"));
    unavailable!(store.robots().list(&RobotFilter::default(), &page(1)));
    unavailable!(store.robots().purge_expired(at));
    unavailable!(store.robots().list_expiring(Duration::from_secs(1), at, 1));
    unavailable!(store.quotas().get(QuotaScope::Project(project)));
    unavailable!(store.quotas().headroom(RepositoryId::generate()));
    unavailable!(store.quotas().recount(QuotaScope::Project(project)));
    unavailable!(store.quotas().list(&page(1)));
    unavailable!(
        store
            .quotas()
            .set_limit(QuotaScope::Project(project), None, 80, None, &actor())
    );
    unavailable!(store.immutability().get(ImmutabilityRuleId::generate()));
    unavailable!(store.immutability().list(PolicyScope::System, &page(1)));
    unavailable!(store.immutability().delete(ImmutabilityRuleId::generate()));
    unavailable!(
        store
            .immutability()
            .covering_rule(RepositoryId::generate(), ItemKind::Path, "x")
    );
    unavailable!(store.holds().get(LifecycleHoldId::generate()));
    unavailable!(store.holds().list(None, false, &page(1)));
    unavailable!(store.holds().active_for(ComponentId::generate()));
    unavailable!(store.cleanup_policies().get(CleanupPolicyId::generate()));
    unavailable!(store.cleanup_policies().list());
    unavailable!(
        store
            .cleanup_policies()
            .repositories_using(CleanupPolicyId::generate())
    );
    unavailable!(store.retention().get(RetentionPolicyId::generate()));
    unavailable!(store.retention().get_for_scope(PolicyScope::System));
    unavailable!(
        store
            .lifecycle()
            .candidates(LifecycleScope::Project(project), &page(1))
    );
    unavailable!(store.lifecycle().apply_deletions(
        LifecycleRunId::generate(),
        &[],
        &WriteOptions::new()
    ));
    unavailable!(store.lifecycle_runs().get(LifecycleRunId::generate()));
    unavailable!(
        store
            .lifecycle_runs()
            .list(None, None::<PolicyRef>, &page(1))
    );
    unavailable!(store.lifecycle_runs().purge(at));
    unavailable!(store.gc_runs().get(GcRunId::generate()));
    unavailable!(store.gc_runs().list(&page(1)));
    unavailable!(store.gc_runs().last_manual_start());
    unavailable!(store.gc_runs().stop_requested(GcRunId::generate()));
    unavailable!(store.download_stats().daily(AssetId::generate(), 1));
    unavailable!(store.lifecycle_runs().finish(
        LifecycleRunId::generate(),
        TaskRunStatus::Failed,
        &LifecycleCounts::default(),
        false,
        at
    ));
}

#[tokio::test]
async fn a_switched_off_store_fails_scans_distribution_storage_and_system_ports() {
    let store = MemStore::new();
    store.set_unavailable(true);
    let at = T0;
    let project = ProjectId::generate();
    let hash = "a".repeat(64);
    unavailable!(store.scanners().get(ScannerId::generate()));
    unavailable!(store.scanners().list());
    unavailable!(store.scans().get_report(ScanReportId::generate()));
    unavailable!(store.scans().state(ComponentId::generate()));
    unavailable!(store.scans().states(&[]));
    unavailable!(store.scans().findings(
        ComponentId::generate(),
        &FindingFilter::default(),
        &page(1)
    ));
    unavailable!(
        store
            .scans()
            .effective_summary(ComponentId::generate(), &[])
    );
    unavailable!(store.scans().stale_components(at, None, 1));
    unavailable!(
        store
            .allowlist()
            .active_ids(ComponentId::generate(), AllowlistMode::System)
    );
    unavailable!(store.allowlist().take_expired_unaudited(at, 1));
    unavailable!(store.allowlist().copy_system_to_project(project, None));
    unavailable!(
        store
            .security_policies()
            .get_scan_policy(PolicyScope::System)
    );
    unavailable!(
        store
            .security_policies()
            .effective_for(RepositoryId::generate())
    );
    unavailable!(
        store
            .security_policies()
            .get_trust_policy(PolicyScope::System)
    );
    unavailable!(
        store
            .security_policies()
            .effective_trust_for(RepositoryId::generate())
    );
    unavailable!(store.trust().get(TrustMaterialId::generate()));
    unavailable!(store.trust().list(None));
    unavailable!(store.hub().stats());
    unavailable!(store.hub().search("CVE-1", &HubFilter::default(), &page(1)));
    unavailable!(store.hub().refresh_stats(at));
    unavailable!(store.exports().get(ExportId::generate()));
    unavailable!(store.exports().due_for_purge(at, 1));
    unavailable!(store.datasets().current(ScannerKind::Trivy, "db"));
    unavailable!(store.datasets().prunable(ScannerKind::Trivy, "db"));
    unavailable!(store.datasets().find_scan_token(&hash, at));
    unavailable!(store.registries().get(RegistryId::generate()));
    unavailable!(store.registries().list());
    unavailable!(store.registries().dependents(RegistryId::generate()));
    unavailable!(store.replication().get_rule(ReplicationRuleId::generate()));
    unavailable!(store.replication().list_rules());
    unavailable!(
        store
            .replication()
            .get_execution(ReplicationExecutionId::generate())
    );
    unavailable!(store.replication().list_executions(None, &page(1)));
    unavailable!(store.replication().claim_task("o", Duration::from_secs(1)));
    unavailable!(
        store
            .replication()
            .tasks(ReplicationExecutionId::generate(), &page(1))
    );
    unavailable!(
        store
            .replication()
            .read_task_log(ReplicationTaskId::generate(), 0, 1)
    );
    unavailable!(store.replication().settle_executions(at));
    unavailable!(store.replication().purge(at));
    unavailable!(store.webhooks().get(WebhookId::generate()));
    unavailable!(store.webhooks().list(None));
    unavailable!(
        store
            .webhooks()
            .any_subscribes_to(rustryx_core::events::EventKind::Push)
    );
    unavailable!(
        store
            .webhooks()
            .matching(rustryx_core::events::EventKind::Push, None, None)
    );
    unavailable!(store.deliveries().pending_count());
    unavailable!(store.deliveries().claim("o", Duration::from_secs(1)));
    unavailable!(store.deliveries().list(WebhookId::generate(), &page(1)));
    unavailable!(store.deliveries().purge(at));
    unavailable!(store.notifications().list(None));
    unavailable!(store.notifications().delete(SubscriptionId::generate()));
    unavailable!(store.notifications().count_sent_since(at));
    unavailable!(store.notifications().claim("o", Duration::from_secs(1)));
    unavailable!(store.promotions().get(PromotionId::generate()));
    unavailable!(store.promotions().get_target(RepositoryId::generate()));
    unavailable!(store.promotions().list(None, None, &page(1)));
    unavailable!(store.promotions().decisions(PromotionId::generate()));
    unavailable!(store.promotions().expire(at));
    unavailable!(store.store_members().members(BlobStoreId::generate()));
    unavailable!(store.store_members().group_of(BlobStoreId::generate()));
    unavailable!(store.blob_moves().due_for_cleanup(at, 1));
    unavailable!(
        store
            .blob_moves()
            .mark_cleaned(BlobId::generate(), BlobStoreId::generate())
    );
    unavailable!(store.verify_runs().get(VerifyRunId::generate()));
    unavailable!(store.verify_runs().list(None, &page(1)));
    unavailable!(store.verify_runs().drop_listing(VerifyRunId::generate()));
    unavailable!(store.store_stats().list());
    unavailable!(store.store_stats().get(BlobStoreId::generate()));
    unavailable!(store.outbox().read_batch("c", 1));
    unavailable!(store.outbox().lag("c"));
    unavailable!(store.outbox().purge(at));
    unavailable!(store.outbox().advance(&CursorAdvance {
        consumer: "c",
        from: OutboxCursor::default(),
        to: OutboxCursor::default()
    }));
    unavailable!(store.task_definitions().list(None));
    unavailable!(store.task_definitions().get(TaskDefinitionId::generate()));
    unavailable!(store.task_definitions().due(at, 1));
    unavailable!(store.task_runs().get(TaskRunId::generate()));
    unavailable!(store.task_runs().list(None, &page(1)));
    unavailable!(store.task_runs().claim("n", &["k"], Duration::from_secs(1)));
    unavailable!(store.task_runs().purge(at));
    unavailable!(NodeRegistryRepo::list_live(&store, Duration::from_secs(1)));
    unavailable!(store.config().get("settings.x", SecretView::Mask));
    unavailable!(store.config().list_prefix("settings.", SecretView::Reveal));
    unavailable!(store.config().current_revision());
    unavailable!(store.config().delete("settings.x", None));
    unavailable!(store.certificates().list());
    unavailable!(store.certificates().get(TrustedCertificateId::generate()));
    unavailable!(
        store
            .certificates()
            .dependents(TrustedCertificateId::generate())
    );
    assert!(store.set_read_only(true, "x").is_err());
}
