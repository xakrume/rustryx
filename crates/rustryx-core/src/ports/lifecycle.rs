//! Ports of quotas, immutability, holds, cleanup and retention policies, lifecycle runs and
//! garbage collection runs.
//!
//! Contract as in [`crate::ports`]: every method is one statement or one transaction and
//! therefore cancel safe (a cancelled future leaves the old or the new state, never a mix),
//! unless its doc says otherwise. Methods that enforce policies are atomic.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use time::{Date, OffsetDateTime};

use crate::audit::ActorRef;
use crate::error::StoreError;
use crate::ids::{
    AssetId, CleanupPolicyId, ComponentId, GcRunId, ImmutabilityRuleId, LifecycleHoldId,
    LifecycleRunId, ProjectId, RepositoryId, RetentionPolicyId, UserId,
};
use crate::lifecycle::{
    CleanupPolicy, GcCounts, GcRun, GcSample, HoldTarget, ImmutabilityRule, ItemDecision, ItemKind,
    LifecycleCounts, LifecycleHold, LifecycleItem, LifecycleKind, LifecycleRun, PolicyScope,
    RetentionPolicy,
};
use crate::model::WriteOptions;
use crate::ports::repos::{Page, PageRequest};
use crate::quota::{Headroom, QuotaScope, QuotaUsage};
use crate::tasks::TaskRunStatus;

/// Quotas of projects and repositories.
#[async_trait]
pub trait QuotaRepo: Send + Sync {
    /// Limit and usage; `None` when the scope has neither (no row, no limit).
    async fn get(&self, scope: QuotaScope) -> Result<Option<QuotaUsage>, StoreError>;
    /// Project scope writes `quotas` (and the mirror in `quota_usage`); repository scope writes
    /// `repositories.quota_bytes`. `Conflict` on a stale `expected_version`.
    async fn set_limit(
        &self,
        scope: QuotaScope,
        limit_bytes: Option<u64>,
        warn_percent: u8,
        expected_version: Option<i64>,
        actor: &ActorRef,
    ) -> Result<(), StoreError>;
    /// The pre check: the smallest remaining room over the repository and its project, from
    /// the counters. Used before the body is read, with the declared length. It is advisory:
    /// the authority is `AssetRepo::publish_with`.
    async fn headroom(&self, repository: RepositoryId) -> Result<Headroom, StoreError>;
    /// Recomputes the usage from the assets (distinct blobs) with an extended statement
    /// timeout and corrects the counter. Returns `(before, after)`. A difference is counted in
    /// `quota_recount_corrections_total`.
    async fn recount(&self, scope: QuotaScope) -> Result<(u64, u64), StoreError>;
    /// Lists scopes with a counter, keyset paged.
    async fn list(&self, page: &PageRequest) -> Result<Page<QuotaUsage>, StoreError>;
}

/// Immutability rules.
#[async_trait]
pub trait ImmutabilityRepo: Send + Sync {
    /// Inserts. At most 15 per scope: `Conflict` beyond.
    async fn create(&self, rule: &ImmutabilityRule) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: ImmutabilityRuleId) -> Result<Option<ImmutabilityRule>, StoreError>;
    /// Lists the rules of a scope.
    async fn list(
        &self,
        scope: PolicyScope,
        page: &PageRequest,
    ) -> Result<Page<ImmutabilityRule>, StoreError>;
    /// Replaces a rule. `Conflict` on a stale `expected_version`.
    async fn update(
        &self,
        rule: &ImmutabilityRule,
        expected_version: i64,
    ) -> Result<(), StoreError>;
    /// Deletes a rule (audited by the caller: removing a rule is how an item becomes
    /// changeable again).
    async fn delete(&self, id: ImmutabilityRuleId) -> Result<(), StoreError>;
    /// The first enabled rule that covers the item, or `None`. Lifecycle jobs, promotion and
    /// the UI preview use it; publishing uses the SQL guard itself, not this call.
    async fn covering_rule(
        &self,
        repository: RepositoryId,
        kind: ItemKind,
        item: &str,
    ) -> Result<Option<ImmutabilityRuleId>, StoreError>;
    /// Up to `limit` existing items the rule would cover, for the editor preview.
    async fn preview(&self, rule: &ImmutabilityRule, limit: u32)
    -> Result<Vec<String>, StoreError>;
}

/// Holds.
#[async_trait]
pub trait LifecycleHoldRepo: Send + Sync {
    /// Inserts.
    async fn create(&self, hold: &LifecycleHold) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: LifecycleHoldId) -> Result<Option<LifecycleHold>, StoreError>;
    /// Releases a hold. `NotFound` when absent; idempotent for a released one.
    async fn release(
        &self,
        id: LifecycleHoldId,
        by: Option<UserId>,
        at: OffsetDateTime,
    ) -> Result<(), StoreError>;
    /// Lists holds, optionally of one target and only active ones.
    async fn list(
        &self,
        target: Option<HoldTarget>,
        active_only: bool,
        page: &PageRequest,
    ) -> Result<Page<LifecycleHold>, StoreError>;
    /// Active holds that cover a component (its own, its repository's, its project's).
    async fn active_for(&self, component: ComponentId) -> Result<Vec<LifecycleHold>, StoreError>;
}

/// Cleanup policies (global objects attached to repositories by id).
#[async_trait]
pub trait CleanupPolicyRepo: Send + Sync {
    /// Inserts. `Conflict` when the name exists.
    async fn create(&self, policy: &CleanupPolicy) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: CleanupPolicyId) -> Result<Option<CleanupPolicy>, StoreError>;
    /// Lists all policies ordered by name.
    async fn list(&self) -> Result<Vec<CleanupPolicy>, StoreError>;
    /// Replaces a policy. `Conflict` on a stale `expected_version`.
    async fn update(&self, policy: &CleanupPolicy, expected_version: i64)
    -> Result<(), StoreError>;
    /// `Conflict` while a repository lists the policy in `cleanup_policy_ids` (the array has
    /// no foreign key, so the port checks it with the GIN index and the caller shows
    /// `repositories_using`).
    async fn delete(&self, id: CleanupPolicyId) -> Result<(), StoreError>;
    /// The repositories that list the policy.
    async fn repositories_using(
        &self,
        id: CleanupPolicyId,
    ) -> Result<Vec<RepositoryId>, StoreError>;
}

/// Retention policies, at most one per scope.
#[async_trait]
pub trait RetentionPolicyRepo: Send + Sync {
    /// Inserts or replaces the policy of its scope. With `expected_version`, `Conflict` on a
    /// stale one; without it, `Conflict` when a policy of that scope already exists.
    async fn upsert(
        &self,
        policy: &RetentionPolicy,
        expected_version: Option<i64>,
    ) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: RetentionPolicyId) -> Result<Option<RetentionPolicy>, StoreError>;
    /// The policy of a scope.
    async fn get_for_scope(
        &self,
        scope: PolicyScope,
    ) -> Result<Option<RetentionPolicy>, StoreError>;
    /// Deletes a policy.
    async fn delete(&self, id: RetentionPolicyId) -> Result<(), StoreError>;
}

/// Where the engine reads candidates from.
#[derive(Debug, Clone, Copy)]
pub enum LifecycleScope {
    /// One repository.
    Repository(RepositoryId),
    /// Every repository of a project.
    Project(ProjectId),
}

/// One component with everything a policy can ask about it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Candidate {
    /// The component.
    pub component_id: ComponentId,
    /// Its repository.
    pub repository_id: RepositoryId,
    /// Repository name.
    pub repository: String,
    /// Namespace.
    pub group_id: Option<String>,
    /// Package name.
    pub name: String,
    /// Version.
    pub version: Option<String>,
    /// OCI tags pointing at this component; empty for other formats and for untagged
    /// manifests.
    pub tags: Vec<String>,
    /// Size of the component.
    pub size_bytes: u64,
    /// When it was published.
    pub published_at: OffsetDateTime,
    /// Latest push of any tag (OCI) or `published_at`.
    pub pushed_at: OffsetDateTime,
    /// Last client download.
    pub pulled_at: Option<OffsetDateTime>,
    /// Signature or SBOM referrer: follows its subject and is never a candidate on its own.
    pub is_referrer: bool,
}

impl Candidate {
    /// A candidate with no namespace, version, tags or download, pushed when it was
    /// published, and not a referrer.
    #[must_use]
    pub fn new(
        component_id: ComponentId,
        repository_id: RepositoryId,
        repository: impl Into<String>,
        name: impl Into<String>,
        published_at: OffsetDateTime,
    ) -> Self {
        Self {
            component_id,
            repository_id,
            repository: repository.into(),
            group_id: None,
            name: name.into(),
            version: None,
            tags: Vec::new(),
            size_bytes: 0,
            published_at,
            pushed_at: published_at,
            pulled_at: None,
            is_referrer: false,
        }
    }
}

/// What happened to one component of a deletion batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteResult {
    /// Deleted; the bytes it held.
    Deleted {
        /// Size of the deleted component.
        bytes: u64,
    },
    /// Not deleted because a guard protects it. The batch goes on.
    Skipped(ItemDecision),
    /// The delete failed.
    Failed,
}

/// The engine's reads and deletions.
#[async_trait]
pub trait LifecycleRepo: Send + Sync {
    /// Keyset paging over components ordered by
    /// `(repository_id, group_id, name, published_at DESC, id)`, so a streaming engine holds
    /// only the versions of one package at a time (more than 100 000 versions of one name is
    /// `Invalid`). Referrers are excluded. Cancel safe, read only.
    async fn candidates(
        &self,
        scope: LifecycleScope,
        page: &PageRequest,
    ) -> Result<Page<Candidate>, StoreError>;
    /// Deletes a batch (at most 500) in one transaction. Inside it, for each component: the
    /// immutability guard and legal hold guard run again unless `options.override_immutable`,
    /// referrers are deleted with their subject, the rows of `lifecycle_run_items` for this
    /// batch are written, and the outbox events of `options.events` are inserted. A protected
    /// component is reported as `Skipped`, it does not abort the batch. Returns one result per
    /// input, in order.
    async fn apply_deletions(
        &self,
        run: LifecycleRunId,
        batch: &[ComponentId],
        options: &WriteOptions,
    ) -> Result<Vec<DeleteResult>, StoreError>;
}

/// The policy a run belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyRef {
    /// A cleanup policy, optionally for one repository.
    Cleanup(CleanupPolicyId, Option<RepositoryId>),
    /// A retention policy.
    Retention(RetentionPolicyId),
}

/// Lifecycle run history.
#[async_trait]
pub trait LifecycleRunRepo: Send + Sync {
    /// Inserts a run.
    async fn create(&self, run: &LifecycleRun) -> Result<(), StoreError>;
    /// Updates the counters of a running run.
    async fn update_progress(
        &self,
        id: LifecycleRunId,
        counts: &LifecycleCounts,
    ) -> Result<(), StoreError>;
    /// Ends a run.
    async fn finish(
        &self,
        id: LifecycleRunId,
        status: TaskRunStatus,
        counts: &LifecycleCounts,
        truncated: bool,
        at: OffsetDateTime,
    ) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: LifecycleRunId) -> Result<Option<LifecycleRun>, StoreError>;
    /// Lists runs, newest first.
    async fn list(
        &self,
        kind: Option<LifecycleKind>,
        policy: Option<PolicyRef>,
        page: &PageRequest,
    ) -> Result<Page<LifecycleRun>, StoreError>;
    /// Stores plan items of a dry run, at most 100 000 per run; returns `true` when the cap
    /// cut the list.
    async fn save_items(
        &self,
        run: LifecycleRunId,
        items: &[LifecycleItem],
    ) -> Result<bool, StoreError>;
    /// Lists the items of a run.
    async fn list_items(
        &self,
        run: LifecycleRunId,
        page: &PageRequest,
    ) -> Result<Page<LifecycleItem>, StoreError>;
    /// Streams every item, for the CSV export. Backpressured; a dropped stream ends the query.
    fn stream_items(
        &self,
        run: LifecycleRunId,
    ) -> BoxStream<'static, Result<LifecycleItem, StoreError>>;
    /// The newest finished dry run of that policy at that version, started less than
    /// `max_age` ago. The gate of the real run.
    async fn last_dry_run(
        &self,
        policy: PolicyRef,
        policy_version: i64,
        max_age: Duration,
    ) -> Result<Option<LifecycleRun>, StoreError>;
    /// Removes runs finished before the cutoff; returns how many.
    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError>;
}

/// Garbage collection history.
#[async_trait]
pub trait GcRunRepo: Send + Sync {
    /// Inserts a run.
    async fn create(&self, run: &GcRun) -> Result<(), StoreError>;
    /// Finds by id.
    async fn get(&self, id: GcRunId) -> Result<Option<GcRun>, StoreError>;
    /// Lists runs, newest first.
    async fn list(&self, page: &PageRequest) -> Result<Page<GcRun>, StoreError>;
    /// Updates the counters of a running run.
    async fn update_progress(
        &self,
        id: GcRunId,
        counts: &GcCounts,
        per_store: &serde_json::Value,
    ) -> Result<(), StoreError>;
    /// Ends a run.
    async fn finish(
        &self,
        id: GcRunId,
        status: TaskRunStatus,
        at: OffsetDateTime,
    ) -> Result<(), StoreError>;
    /// Sets the flag; the sweep reads it before every batch. Returns `false` for a finished
    /// run.
    async fn request_stop(&self, id: GcRunId) -> Result<bool, StoreError>;
    /// Whether a stop was requested.
    async fn stop_requested(&self, id: GcRunId) -> Result<bool, StoreError>;
    /// Dry run report sample, capped at 1000 rows per run.
    async fn save_samples(&self, samples: &[GcSample]) -> Result<(), StoreError>;
    /// Lists the sample of a run.
    async fn list_samples(
        &self,
        run: GcRunId,
        page: &PageRequest,
    ) -> Result<Page<GcSample>, StoreError>;
    /// Start time of the newest manual run, for the one per minute limit.
    async fn last_manual_start(&self) -> Result<Option<OffsetDateTime>, StoreError>;
    /// Removes runs finished before the cutoff; returns how many.
    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError>;
}

/// Read side of the daily download counters.
#[async_trait]
pub trait DownloadStatsRepo: Send + Sync {
    /// Daily counts of the last `days` days (at most 90), oldest first, gaps filled with zero.
    async fn daily(&self, asset: AssetId, days: u32) -> Result<Vec<(Date, u64)>, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::at;

    #[test]
    fn candidate_constructor_sets_the_documented_defaults() {
        let candidate = Candidate::new(
            ComponentId::generate(),
            RepositoryId::generate(),
            "app",
            "web",
            at(),
        );
        assert_eq!(candidate.repository, "app");
        assert_eq!(candidate.name, "web");
        assert!(candidate.group_id.is_none() && candidate.version.is_none());
        assert_eq!(candidate.tags.len(), 0);
        assert_eq!(candidate.size_bytes, 0);
        assert_eq!(
            candidate.pushed_at, candidate.published_at,
            "pushed when published"
        );
        assert!(candidate.pulled_at.is_none());
        assert!(
            !candidate.is_referrer,
            "a new candidate is a subject, not a referrer"
        );
    }

    #[test]
    fn delete_results_distinguish_deleted_skipped_and_failed() {
        let deleted = DeleteResult::Deleted { bytes: 10 };
        assert_eq!(deleted, DeleteResult::Deleted { bytes: 10 });
        assert_ne!(deleted, DeleteResult::Deleted { bytes: 11 });
        assert_ne!(deleted, DeleteResult::Failed);
        assert_ne!(
            DeleteResult::Skipped(ItemDecision::ProtectedHold),
            DeleteResult::Skipped(ItemDecision::ProtectedImmutable)
        );
    }

    #[test]
    fn policy_refs_compare_by_policy_and_repository() {
        let policy = CleanupPolicyId::generate();
        let repo = RepositoryId::generate();
        assert_eq!(
            PolicyRef::Cleanup(policy, Some(repo)),
            PolicyRef::Cleanup(policy, Some(repo))
        );
        assert_ne!(
            PolicyRef::Cleanup(policy, Some(repo)),
            PolicyRef::Cleanup(policy, None)
        );
    }
}
