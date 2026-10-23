//! Tables behind [`super::MemStore`].

use std::collections::{BTreeMap, HashMap, HashSet};

use time::{Date, OffsetDateTime};

use rustryx_core::events::EventKind;
use rustryx_core::events::{OutboxCursor, OutboxEvent};
use rustryx_core::identity::{
    ApiToken, ContentSelector, IdpGroupMapping, Privilege, Role, RoleOrigin, User,
};
use rustryx_core::ids::{
    ApiTokenId, AssetId, BlobId, BlobStoreId, CleanupPolicyId, ComponentId, ContentSelectorId,
    CveAllowlistEntryId, ExportId, GcRunId, IdpGroupMappingId, ImmutabilityRuleId, LabelId,
    LifecycleHoldId, LifecycleRunId, PrivilegeId, ProjectId, ProjectMemberId, PromotionId,
    RegistryId, ReplicationRuleId, ReplicationTaskId, RepositoryId, RetentionPolicyId,
    RobotTokenId, RoleId, RoutingRuleId, ScanReportId, ScannerDataSetId, ScannerId,
    SecurityPolicyId, SubscriptionId, TaskDefinitionId, TaskRunId, TrustMaterialId, TrustPolicyId,
    TrustedCertificateId, UserId, VerifyRunId, WebhookId,
};
use rustryx_core::label::{ComponentLabel, Label};
use rustryx_core::lifecycle::{
    CleanupPolicy, GcRun, GcSample, ImmutabilityRule, LifecycleHold, LifecycleItem, LifecycleRun,
    RetentionPolicy,
};
use rustryx_core::model::{Asset, BlobRecord, BlobStoreDef, Component, Repository};
use rustryx_core::node::NodeInfo;
use rustryx_core::notification::{EmailDelivery, NotificationSubscription};
use rustryx_core::ports::{ExportJob, SecurityStats};
use rustryx_core::ports::{LoginKey, LoginState};
use rustryx_core::project::Project;
use rustryx_core::project::ProjectMember;
use rustryx_core::promotion::{
    PromotionDecision, PromotionItem, PromotionRequest, PromotionTarget,
};
use rustryx_core::quota::{Quota, QuotaScope};
use rustryx_core::registry::Registry;
use rustryx_core::replication::{ReplicationExecution, ReplicationRule, ReplicationTask};
use rustryx_core::robot::RobotToken;
use rustryx_core::routing::RoutingRule;
use rustryx_core::scan::{
    ComponentSecurityState, CveAllowlistEntry, Finding, ScanPolicy, ScanReport, ScannerDataSet,
    ScannerRecord, TrustMaterial, TrustPolicy,
};
use rustryx_core::settings::TrustedCertificate;
use rustryx_core::storage::{BlobMove, BlobStoreMember, BlobStoreStats, VerifyFinding, VerifyRun};
use rustryx_core::tasks::TaskLogLine;
use rustryx_core::tasks::{TaskDefinition, TaskRun};
use rustryx_core::webhook::{Webhook, WebhookDelivery};

#[derive(Default)]
pub(crate) struct State {
    /// The store clock at the moment the lock was taken (`MemStore::tables` sets it). Row
    /// level helpers such as `remove_asset` stamp orphaned blobs with it, so they need no
    /// time argument and every write of one call sees one instant.
    pub(crate) now: Stamp,
    /// When a blob lost its last reference (`blobs.orphaned_at`). A blob that was never
    /// referenced has no entry and ages from its creation, as `COALESCE(orphaned_at,
    /// created_at)` does in the real GC queries.
    pub(crate) orphaned_at: HashMap<BlobId, OffsetDateTime>,
    pub(crate) repositories: HashMap<RepositoryId, Repository>,
    pub(crate) blob_stores: HashMap<BlobStoreId, BlobStoreDef>,
    pub(crate) blobs: HashMap<BlobId, BlobRecord>,
    pub(crate) components: HashMap<ComponentId, Component>,
    pub(crate) assets: HashMap<AssetId, Asset>,
    /// Ordered `(repository, path)` index: gives exact lookup and prefix scans, and
    /// keeps path uniqueness a property of the structure rather than of every writer.
    pub(crate) asset_index: BTreeMap<(RepositoryId, String), AssetId>,
    pub(crate) users: HashMap<UserId, User>,
    /// Role assignments per user. One role may appear under several origins.
    pub(crate) user_roles: HashMap<UserId, Vec<(RoleId, RoleOrigin)>>,
    pub(crate) roles: HashMap<RoleId, Role>,
    pub(crate) privileges: HashMap<PrivilegeId, Privilege>,
    pub(crate) selectors: HashMap<ContentSelectorId, ContentSelector>,
    pub(crate) tokens: HashMap<ApiTokenId, ApiToken>,
    pub(crate) mappings: HashMap<IdpGroupMappingId, IdpGroupMapping>,
    pub(crate) login_attempts: HashMap<LoginKey, LoginState>,
    /// Outbox rows with the id of the "transaction" that wrote them, in write order. The
    /// store has no transactions, so every state changing call is one: it takes the next
    /// `tx_id` once and stamps all its events with it.
    pub(crate) outbox: Vec<(u64, OutboxEvent)>,
    pub(crate) outbox_seq: i64,
    pub(crate) tx_seq: u64,
    pub(crate) outbox_cursors: HashMap<String, OutboxCursor>,
    /// `system_config` rows by key.
    pub(crate) config: BTreeMap<String, ConfigRow>,
    pub(crate) config_revision: i64,
    /// Daily download counters of assets.
    pub(crate) downloads: BTreeMap<(AssetId, Date), u64>,
    /// Revoked internal JWT ids with the `exp` after which a row is useless.
    pub(crate) revoked_tokens: HashMap<uuid::Uuid, OffsetDateTime>,
    /// Durable queue rows in insertion order (the order `claim` serves).
    pub(crate) jobs: Vec<JobRow>,
    pub(crate) routing_rules: HashMap<RoutingRuleId, RoutingRule>,
    pub(crate) projects: HashMap<ProjectId, Project>,
    pub(crate) members: HashMap<ProjectMemberId, ProjectMember>,
    pub(crate) labels: HashMap<LabelId, Label>,
    pub(crate) component_labels: HashMap<(ComponentId, LabelId), ComponentLabel>,
    pub(crate) robots: HashMap<RobotTokenId, RobotToken>,
    /// Robots and personal tokens that already got their expiry notice.
    pub(crate) robot_notified: HashMap<RobotTokenId, OffsetDateTime>,
    pub(crate) token_notified: HashMap<ApiTokenId, OffsetDateTime>,
    pub(crate) cleanup_policies: HashMap<CleanupPolicyId, CleanupPolicy>,
    pub(crate) retention_policies: HashMap<RetentionPolicyId, RetentionPolicy>,
    /// Lifecycle runs with a creation counter, so "newest first" does not depend on ids.
    pub(crate) lifecycle_runs: Vec<(u64, LifecycleRun)>,
    pub(crate) lifecycle_items: HashMap<LifecycleRunId, Vec<LifecycleItem>>,
    pub(crate) gc_runs: Vec<GcRunRow>,
    pub(crate) gc_samples: HashMap<GcRunId, Vec<GcSample>>,
    /// Creation counter shared by the run tables.
    pub(crate) run_seq: u64,
    pub(crate) scanners: HashMap<ScannerId, ScannerRecord>,
    /// Scan reports in creation order.
    pub(crate) scan_reports: Vec<ScanReport>,
    /// Findings of the current reports only, as in the real schema.
    pub(crate) findings: HashMap<ScanReportId, Vec<Finding>>,
    pub(crate) security_states: HashMap<ComponentId, ComponentSecurityState>,
    pub(crate) scan_policies: HashMap<SecurityPolicyId, ScanPolicy>,
    pub(crate) trust_policies: HashMap<TrustPolicyId, TrustPolicy>,
    pub(crate) trust_materials: HashMap<TrustMaterialId, TrustMaterial>,
    pub(crate) allowlist: HashMap<CveAllowlistEntryId, CveAllowlistEntry>,
    /// Entries whose expiry was already handed to `take_expired_unaudited`.
    pub(crate) allowlist_audited: HashSet<CveAllowlistEntryId>,
    pub(crate) security_stats: Option<SecurityStats>,
    pub(crate) exports: HashMap<ExportId, ExportJob>,
    pub(crate) datasets: HashMap<ScannerDataSetId, ScannerDataSet>,
    /// Scan access tokens by hash, with the scan, the repository and the expiry.
    pub(crate) scan_tokens: HashMap<String, (ScanReportId, RepositoryId, OffsetDateTime)>,
    pub(crate) store_members: Vec<BlobStoreMember>,
    /// The move ledger by `(blob, from store)`.
    pub(crate) blob_moves: HashMap<(BlobId, BlobStoreId), BlobMove>,
    pub(crate) verify_runs: Vec<(u64, VerifyRun)>,
    /// Staged object listing per run: key to `(size, modified)`.
    pub(crate) verify_listing: HashMap<VerifyRunId, BTreeMap<String, (u64, OffsetDateTime)>>,
    pub(crate) verify_findings: HashMap<VerifyRunId, Vec<VerifyFinding>>,
    pub(crate) store_stats: HashMap<BlobStoreId, BlobStoreStats>,
    pub(crate) registries: HashMap<RegistryId, Registry>,
    pub(crate) replication_rules: HashMap<ReplicationRuleId, ReplicationRule>,
    pub(crate) executions: Vec<(u64, ReplicationExecution)>,
    pub(crate) replication_tasks: Vec<TaskRow>,
    pub(crate) replication_logs: HashMap<ReplicationTaskId, Vec<TaskLogLine>>,
    pub(crate) webhooks: HashMap<WebhookId, Webhook>,
    pub(crate) webhook_deliveries: Vec<DeliveryRow>,
    pub(crate) subscriptions: HashMap<SubscriptionId, NotificationSubscription>,
    pub(crate) emails: Vec<EmailRow>,
    pub(crate) promotion_targets: HashMap<RepositoryId, PromotionTarget>,
    pub(crate) promotions: Vec<(u64, PromotionRequest)>,
    pub(crate) promotion_items: HashMap<PromotionId, Vec<PromotionItem>>,
    pub(crate) promotion_decisions: HashMap<PromotionId, Vec<PromotionDecision>>,
    pub(crate) task_definitions: HashMap<TaskDefinitionId, TaskDefinition>,
    pub(crate) task_runs: Vec<TaskRunRow>,
    pub(crate) task_logs: HashMap<TaskRunId, Vec<TaskLogLine>>,
    pub(crate) nodes: HashMap<String, NodeInfo>,
    pub(crate) trusted_certificates: HashMap<TrustedCertificateId, TrustedCertificate>,
    pub(crate) immutability_rules: HashMap<ImmutabilityRuleId, ImmutabilityRule>,
    pub(crate) holds: HashMap<LifecycleHoldId, LifecycleHold>,
    /// Limits of project scope quotas (`quotas` table). A repository limit lives in
    /// `Repository::quota_bytes`, as in the real schema.
    pub(crate) project_quotas: HashMap<ProjectId, Quota>,
    /// Warn percentage of repository scope quotas (`quota_usage.warn_percent`; a project
    /// keeps its own in `Quota::warn_percent`).
    pub(crate) repository_warn: HashMap<RepositoryId, u8>,
    /// Scopes whose `quota.near_threshold` edge already fired (`quota_usage.warned`).
    pub(crate) quota_warned: HashSet<QuotaScope>,
    /// Scopes that were recounted, with the time of the last recount.
    pub(crate) quota_recounted: HashMap<QuotaScope, OffsetDateTime>,
}

/// One row of `system_config`. Not `Debug` on purpose: it holds sealed values in the clear.
#[derive(Clone)]
pub(crate) struct ConfigRow {
    /// The value as stored: fields sealed at rest are not in it.
    pub(crate) value: serde_json::Value,
    /// The sealed fields by JSON pointer, in the clear. The real store keeps envelopes; the
    /// double keeps the text and only ever hands it out through `SecretView::Reveal`.
    pub(crate) sealed: BTreeMap<String, String>,
    pub(crate) revision: i64,
    pub(crate) updated_at: OffsetDateTime,
    pub(crate) updated_by: Option<UserId>,
}

impl State {
    /// Removes an asset row and its index entry together, so the two cannot drift.
    pub(crate) fn remove_asset(&mut self, id: AssetId) -> Option<Asset> {
        let asset = self.assets.remove(&id)?;
        self.asset_index
            .remove(&(asset.repository_id, asset.path.clone()));
        self.note_blob_unreferenced(asset.blob_id);
        Some(asset)
    }

    /// The `assets_blob_ref` trigger, release side: once no asset references the blob it
    /// is orphaned from now on. A blob that is still referenced keeps no orphan stamp.
    pub(crate) fn note_blob_unreferenced(&mut self, blob: BlobId) {
        if !self.assets.values().any(|asset| asset.blob_id == blob) {
            let now = self.now.0;
            self.orphaned_at.insert(blob, now);
        }
    }

    /// The age GC measures: since the last reference went away, or since creation for a blob
    /// that was never referenced.
    pub(crate) fn blob_age_start(&self, blob: &BlobRecord) -> OffsetDateTime {
        self.orphaned_at
            .get(&blob.id)
            .copied()
            .unwrap_or(blob.created_at)
    }
}

/// State of a queue row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

/// One row of the durable queue table.
#[derive(Debug, Clone)]
pub(crate) struct JobRow {
    pub(crate) id: uuid::Uuid,
    pub(crate) queue: String,
    pub(crate) dedupe_key: Option<String>,
    pub(crate) payload: serde_json::Value,
    pub(crate) state: JobState,
    pub(crate) attempts: i32,
    pub(crate) owner: Option<String>,
    pub(crate) available_at: OffsetDateTime,
    pub(crate) lease_expires_at: Option<OffsetDateTime>,
    pub(crate) error: Option<String>,
    pub(crate) finished_at: Option<OffsetDateTime>,
}

impl State {
    /// Webhooks scoped to a project, for `ProjectRepo::dependents`.
    pub(crate) fn webhook_count_of_project(&self, project: ProjectId) -> u32 {
        let count = self
            .webhooks
            .values()
            .filter(|w| w.scope == rustryx_core::webhook::NotifyScope::Project(project))
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

/// A garbage collection run with the columns the type does not carry.
#[derive(Debug, Clone)]
pub(crate) struct GcRunRow {
    pub(crate) seq: u64,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) run: GcRun,
}

impl State {
    /// The next value of `system_config_revision_seq`.
    pub(crate) fn next_config_revision(&mut self) -> i64 {
        self.config_revision += 1;
        self.config_revision
    }

    /// Bumps `security.revision`, which every write to a scan policy, an allowlist entry, a
    /// trust policy, trust material and a quarantine change makes in its own transaction.
    /// Returns the `system_config` revision the caller publishes as
    /// `InvalidationEvent::Config`.
    pub(crate) fn bump_security_revision(&mut self, now: OffsetDateTime) -> i64 {
        let current = self
            .config
            .get("security.revision")
            .and_then(|row| row.value.as_i64())
            .unwrap_or(0);
        let revision = self.next_config_revision();
        self.config.insert(
            "security.revision".to_owned(),
            ConfigRow {
                value: serde_json::json!(current.saturating_add(1)),
                sealed: BTreeMap::new(),
                revision,
                updated_at: now,
                updated_by: None,
            },
        );
        revision
    }
}

/// A replication task with the lease columns the type does not carry.
#[derive(Debug, Clone)]
pub(crate) struct TaskRow {
    pub(crate) task: ReplicationTask,
    pub(crate) owner: Option<String>,
    pub(crate) lease_expires_at: Option<OffsetDateTime>,
}

/// A webhook delivery with its creation order and lease.
#[derive(Debug, Clone)]
pub(crate) struct DeliveryRow {
    pub(crate) seq: u64,
    pub(crate) delivery: WebhookDelivery,
    pub(crate) owner: Option<String>,
    pub(crate) lease_expires_at: Option<OffsetDateTime>,
}

/// An e-mail delivery with the columns that suppression and the lease need.
#[derive(Debug, Clone)]
pub(crate) struct EmailRow {
    pub(crate) delivery: EmailDelivery,
    pub(crate) kind: EventKind,
    pub(crate) subject_key: String,
    pub(crate) owner: Option<String>,
    pub(crate) lease_expires_at: Option<OffsetDateTime>,
}

/// A task run with the columns the type does not carry: creation order, lease and attempts.
#[derive(Debug, Clone)]
pub(crate) struct TaskRunRow {
    pub(crate) seq: u64,
    pub(crate) run: TaskRun,
    pub(crate) available_at: OffsetDateTime,
    pub(crate) lease_expires_at: Option<OffsetDateTime>,
    pub(crate) attempts: u8,
}

/// An instant with a `Default`, which `OffsetDateTime` does not have: the epoch, until
/// `MemStore::tables` stamps the real time on every lock.
#[derive(Clone, Copy)]
pub(crate) struct Stamp(pub(crate) OffsetDateTime);

impl Default for Stamp {
    fn default() -> Self {
        Self(OffsetDateTime::UNIX_EPOCH)
    }
}
