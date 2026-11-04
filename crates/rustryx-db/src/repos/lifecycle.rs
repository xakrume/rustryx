//! [`LifecycleRepo`] and [`LifecycleRunRepo`] on PostgreSQL.
//!
//! One `apply_deletions` call is one transaction. Each component delete runs in a savepoint:
//! a policy guard aborts only that statement, and a read only guard aborts the batch.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use rustryx_core::StoreError;
use rustryx_core::error::{MutationError, PolicyViolation};
use rustryx_core::ids::{
    CleanupPolicyId, ComponentId, LifecycleRunId, RepositoryId, RetentionPolicyId, TaskRunId,
    UserId,
};
use rustryx_core::lifecycle::{
    ItemDecision, LifecycleCounts, LifecycleItem, LifecycleKind, LifecycleRun,
    MAX_LIFECYCLE_ITEMS_PER_RUN,
};
use rustryx_core::model::WriteOptions;
use rustryx_core::ports::{
    Candidate, DeleteResult, LifecycleRepo, LifecycleRunRepo, LifecycleScope, Page, PageRequest,
    PolicyRef,
};
use rustryx_core::tasks::TaskRunStatus;
use sqlx::{Acquire, FromRow, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::common::{corrupt, to_i64, to_u64};
use super::write;
use crate::cursor;
use crate::error::{map_err, map_mutation_err};

const DELETE_BATCH_LIMIT: usize = 500;
const DELETE_CONTEXT: &str = "delete component";
const RUN_PAGE: &str = "lifecycle_run";
const ITEM_PAGE: &str = "lifecycle_item";
const CANDIDATE_PAGE: &str = "lifecycle_candidate";
const STREAM_PAGE: i64 = 256;

const TARGET_SQL: &str = "SELECT r.name AS repository, c.group_id, c.name, c.version, \
     COALESCE((SELECT SUM(DISTINCT b.size_bytes)::bigint FROM assets a \
       JOIN blobs b ON b.id = a.blob_id WHERE a.component_id = c.id), 0)::bigint AS size_bytes \
     FROM components c JOIN repositories r ON r.id = c.repository_id WHERE c.id = $1";

const CANDIDATE_SQL: &str = "SELECT c.id, c.repository_id, r.name AS repository, c.group_id, \
     c.name, c.version, \
     COALESCE((SELECT SUM(DISTINCT b.size_bytes)::bigint FROM assets a \
       JOIN blobs b ON b.id = a.blob_id WHERE a.component_id = c.id), 0)::bigint AS size_bytes, \
     c.created_at, c.last_downloaded_at \
     FROM components c JOIN repositories r ON r.id = c.repository_id \
     WHERE (($1::uuid IS NOT NULL AND c.repository_id = $1) \
        OR ($2::uuid IS NOT NULL AND r.project_id = $2)) \
       AND ($3::bool OR c.repository_id > $4 \
        OR (c.repository_id = $4 AND COALESCE(c.group_id, '') > $5) \
        OR (c.repository_id = $4 AND COALESCE(c.group_id, '') = $5 AND c.name > $6) \
        OR (c.repository_id = $4 AND COALESCE(c.group_id, '') = $5 AND c.name = $6 \
            AND c.created_at < $7) \
        OR (c.repository_id = $4 AND COALESCE(c.group_id, '') = $5 AND c.name = $6 \
            AND c.created_at = $7 AND c.id > $8)) \
     ORDER BY c.repository_id, COALESCE(c.group_id, ''), c.name, c.created_at DESC, c.id \
     LIMIT $9";

const RUN_LIST_SQL: &str = "SELECT id, kind, cleanup_policy_id, retention_policy_id, \
     repository_id, dry_run, policy_version, task_run_id, status, counts, truncated, \
     requested_by, confirmed_mass_delete, started_at, finished_at, created_at \
     FROM lifecycle_runs \
     WHERE ($1::text IS NULL OR kind = $1) \
       AND ($2::uuid IS NULL OR cleanup_policy_id = $2) \
       AND (NOT $3 OR repository_id IS NOT DISTINCT FROM $4) \
       AND ($5::uuid IS NULL OR retention_policy_id = $5) \
       AND ($6::bool OR created_at < $7 OR (created_at = $7 AND id < $8)) \
     ORDER BY created_at DESC, id DESC LIMIT $9";

const DRY_SQL: &str = "SELECT id, kind, cleanup_policy_id, retention_policy_id, repository_id, \
     dry_run, policy_version, task_run_id, status, counts, truncated, requested_by, \
     confirmed_mass_delete, started_at, finished_at, created_at FROM lifecycle_runs \
     WHERE dry_run AND policy_version = $1 AND status = 'succeeded' AND finished_at IS NOT NULL \
       AND started_at > $2 \
       AND ($3::uuid IS NULL OR cleanup_policy_id = $3) \
       AND (NOT $4 OR repository_id IS NOT DISTINCT FROM $5) \
       AND ($6::uuid IS NULL OR retention_policy_id = $6) \
     ORDER BY created_at DESC, id DESC LIMIT 1";

/// Deletes components for a lifecycle run and reads deletion candidates.
#[derive(Debug, Clone)]
pub struct PgLifecycleRepo {
    pool: PgPool,
}

impl PgLifecycleRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Lifecycle run history.
#[derive(Debug, Clone)]
pub struct PgLifecycleRunRepo {
    pool: PgPool,
}

impl PgLifecycleRunRepo {
    /// Creates the repository over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, FromRow)]
struct RunRow {
    id: Uuid,
    kind: String,
    cleanup_policy_id: Option<Uuid>,
    retention_policy_id: Option<Uuid>,
    repository_id: Option<Uuid>,
    dry_run: bool,
    policy_version: i64,
    task_run_id: Option<Uuid>,
    status: String,
    counts: serde_json::Value,
    truncated: bool,
    requested_by: Option<Uuid>,
    confirmed_mass_delete: bool,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

#[derive(Debug, FromRow)]
struct ItemRow {
    run_id: Uuid,
    seq: i64,
    repository: String,
    component_id: Uuid,
    coordinate: String,
    tags: Vec<String>,
    size_bytes: i64,
    decision: String,
    reason: String,
}

#[derive(Debug, FromRow)]
struct TargetRow {
    repository: String,
    group_id: Option<String>,
    name: String,
    version: Option<String>,
    size_bytes: i64,
}

#[derive(Debug, FromRow)]
struct CandidateRow {
    id: Uuid,
    repository_id: Uuid,
    repository: String,
    group_id: Option<String>,
    name: String,
    version: Option<String>,
    size_bytes: i64,
    created_at: OffsetDateTime,
    last_downloaded_at: Option<OffsetDateTime>,
}

struct PendingItem {
    repository: String,
    component_id: Uuid,
    coordinate: String,
    size_bytes: i64,
    decision: ItemDecision,
    reason: String,
}

struct Recorded {
    result: DeleteResult,
    item: Option<PendingItem>,
}

fn kind_text(kind: LifecycleKind) -> &'static str {
    match kind {
        LifecycleKind::Cleanup => "cleanup",
        LifecycleKind::Retention => "retention",
        LifecycleKind::LabelCleanup => "label_cleanup",
    }
}

fn parse_kind(raw: &str) -> Result<LifecycleKind, StoreError> {
    match raw {
        "cleanup" => Ok(LifecycleKind::Cleanup),
        "retention" => Ok(LifecycleKind::Retention),
        "label_cleanup" => Ok(LifecycleKind::LabelCleanup),
        _ => Err(corrupt("read lifecycle run")),
    }
}

fn status_text(status: TaskRunStatus) -> Result<&'static str, StoreError> {
    Ok(match status {
        TaskRunStatus::Queued => "queued",
        TaskRunStatus::Running => "running",
        TaskRunStatus::Succeeded => "succeeded",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Skipped => "skipped",
        TaskRunStatus::Abandoned => "abandoned",
        _ => return Err(StoreError::Invalid("unknown task status".to_owned())),
    })
}

fn parse_status(raw: &str) -> Result<TaskRunStatus, StoreError> {
    match raw {
        "queued" => Ok(TaskRunStatus::Queued),
        "running" => Ok(TaskRunStatus::Running),
        "succeeded" => Ok(TaskRunStatus::Succeeded),
        "failed" => Ok(TaskRunStatus::Failed),
        "cancelled" => Ok(TaskRunStatus::Cancelled),
        "skipped" => Ok(TaskRunStatus::Skipped),
        "abandoned" => Ok(TaskRunStatus::Abandoned),
        _ => Err(corrupt("read lifecycle run")),
    }
}

fn decision_text(decision: ItemDecision) -> &'static str {
    match decision {
        ItemDecision::Delete => "delete",
        ItemDecision::DeleteFailed => "delete_failed",
        ItemDecision::ProtectedImmutable => "protected_immutable",
        ItemDecision::ProtectedHold => "protected_hold",
        ItemDecision::ProtectedLastVersion => "protected_last_version",
    }
}

fn parse_decision(raw: &str) -> Result<ItemDecision, StoreError> {
    match raw {
        "delete" => Ok(ItemDecision::Delete),
        "delete_failed" => Ok(ItemDecision::DeleteFailed),
        "protected_immutable" => Ok(ItemDecision::ProtectedImmutable),
        "protected_hold" => Ok(ItemDecision::ProtectedHold),
        "protected_last_version" => Ok(ItemDecision::ProtectedLastVersion),
        _ => Err(corrupt("read lifecycle item")),
    }
}

fn coordinate(group: &Option<String>, name: &str, version: &Option<String>) -> String {
    let mut text = String::new();
    if let Some(group) = group {
        text.push_str(group);
        text.push(':');
    }
    text.push_str(name);
    if let Some(version) = version {
        text.push(':');
        text.push_str(version);
    }
    text
}

fn counts_value(counts: &LifecycleCounts) -> Result<serde_json::Value, StoreError> {
    serde_json::to_value(counts).map_err(|_| corrupt("write lifecycle run"))
}

fn parse_counts(value: serde_json::Value) -> Result<LifecycleCounts, StoreError> {
    if value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(LifecycleCounts::default());
    }
    serde_json::from_value(value).map_err(|_| corrupt("read lifecycle run"))
}

impl RunRow {
    fn into_run(self) -> Result<LifecycleRun, StoreError> {
        let mut run = LifecycleRun::new(
            LifecycleRunId::from_uuid(self.id),
            parse_kind(&self.kind)?,
            self.dry_run,
            self.policy_version,
        );
        run.cleanup_policy_id = self.cleanup_policy_id.map(CleanupPolicyId::from_uuid);
        run.retention_policy_id = self.retention_policy_id.map(RetentionPolicyId::from_uuid);
        run.repository_id = self.repository_id.map(RepositoryId::from_uuid);
        run.task_run_id = self.task_run_id.map(TaskRunId::from_uuid);
        run.status = parse_status(&self.status)?;
        run.counts = parse_counts(self.counts)?;
        run.truncated = self.truncated;
        run.requested_by = self.requested_by.map(UserId::from_uuid);
        run.confirmed_mass_delete = self.confirmed_mass_delete;
        run.started_at = self.started_at;
        run.finished_at = self.finished_at;
        Ok(run)
    }
}

impl ItemRow {
    fn into_item(self) -> Result<LifecycleItem, StoreError> {
        Ok(LifecycleItem {
            run_id: LifecycleRunId::from_uuid(self.run_id),
            seq: to_u64(self.seq, "read lifecycle item")?,
            repository: self.repository,
            component_id: ComponentId::from_uuid(self.component_id),
            coordinate: self.coordinate,
            tags: self.tags,
            size_bytes: to_u64(self.size_bytes, "read lifecycle item")?,
            decision: parse_decision(&self.decision)?,
            reason: self.reason,
        })
    }
}

impl CandidateRow {
    fn into_candidate(self) -> Result<Candidate, StoreError> {
        let mut candidate = Candidate::new(
            ComponentId::from_uuid(self.id),
            RepositoryId::from_uuid(self.repository_id),
            self.repository,
            self.name,
            self.created_at,
        );
        candidate.group_id = self.group_id;
        candidate.version = self.version;
        candidate.size_bytes = to_u64(self.size_bytes, "read lifecycle candidate")?;
        candidate.pulled_at = self.last_downloaded_at;
        Ok(candidate)
    }
}

struct PolicyFilter {
    cleanup: Option<Uuid>,
    require_repository: bool,
    repository: Option<Uuid>,
    retention: Option<Uuid>,
}

fn policy_filter(policy: Option<PolicyRef>) -> PolicyFilter {
    match policy {
        None => PolicyFilter {
            cleanup: None,
            require_repository: false,
            repository: None,
            retention: None,
        },
        Some(PolicyRef::Cleanup(id, repository)) => PolicyFilter {
            cleanup: Some(id.as_uuid()),
            require_repository: repository.is_some(),
            repository: repository.map(|id| id.as_uuid()),
            retention: None,
        },
        Some(PolicyRef::Retention(id)) => PolicyFilter {
            cleanup: None,
            require_repository: false,
            repository: None,
            retention: Some(id.as_uuid()),
        },
    }
}

fn run_cursor(token: &str) -> Result<(bool, OffsetDateTime, Uuid), StoreError> {
    let key = cursor::decode(RUN_PAGE, token)?;
    let (nanos, id) = key.split_once('|').ok_or_else(cursor::invalid_token)?;
    let nanos: i128 = nanos.parse().map_err(|_| cursor::invalid_token())?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| cursor::invalid_token())?;
    let id = Uuid::parse_str(id).map_err(|_| cursor::invalid_token())?;
    Ok((false, at, id))
}

fn candidate_cursor(
    token: &str,
) -> Result<(bool, Uuid, String, String, OffsetDateTime, Uuid), StoreError> {
    let key = cursor::decode(CANDIDATE_PAGE, token)?;
    let mut parts = key.split('\u{1f}');
    let repo = parts.next().ok_or_else(cursor::invalid_token)?;
    let group = parts.next().ok_or_else(cursor::invalid_token)?;
    let name = parts.next().ok_or_else(cursor::invalid_token)?;
    let nanos = parts.next().ok_or_else(cursor::invalid_token)?;
    let id = parts.next().ok_or_else(cursor::invalid_token)?;
    if parts.next().is_some() {
        return Err(cursor::invalid_token());
    }
    let nanos: i128 = nanos.parse().map_err(|_| cursor::invalid_token())?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| cursor::invalid_token())?;
    Ok((
        false,
        Uuid::parse_str(repo).map_err(|_| cursor::invalid_token())?,
        group.to_owned(),
        name.to_owned(),
        at,
        Uuid::parse_str(id).map_err(|_| cursor::invalid_token())?,
    ))
}

async fn scope_exists(pool: &PgPool, scope: LifecycleScope) -> Result<(), StoreError> {
    let (sql, id, kind): (&'static str, Uuid, &'static str) = match scope {
        LifecycleScope::Repository(id) => (
            "SELECT EXISTS(SELECT 1 FROM repositories WHERE id = $1)",
            id.as_uuid(),
            "repository",
        ),
        LifecycleScope::Project(id) => (
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)",
            id.as_uuid(),
            "project",
        ),
    };
    let exists: bool = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|e| map_err("list lifecycle candidates", e))?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::NotFound(kind.to_owned()))
    }
}

fn failed_item(target: &TargetRow, component: Uuid) -> PendingItem {
    PendingItem {
        repository: target.repository.clone(),
        component_id: component,
        coordinate: coordinate(&target.group_id, &target.name, &target.version),
        size_bytes: target.size_bytes,
        decision: ItemDecision::DeleteFailed,
        reason: "failed".to_owned(),
    }
}

fn kept_item(target: &TargetRow, component: Uuid, decision: ItemDecision, reason: String) -> PendingItem {
    PendingItem {
        repository: target.repository.clone(),
        component_id: component,
        coordinate: coordinate(&target.group_id, &target.name, &target.version),
        size_bytes: target.size_bytes,
        decision,
        reason,
    }
}

fn aborted() -> StoreError {
    StoreError::Backend {
        context: DELETE_CONTEXT,
        source: Box::new(crate::error::DbFault::new("deletion transaction aborted")),
    }
}

async fn rollback_savepoint(
    savepoint: Transaction<'_, Postgres>,
    mapped: &MutationError,
) -> Result<(), StoreError> {
    let rolled = savepoint.rollback().await;
    if matches!(mapped, MutationError::Violation(PolicyViolation::ReadOnly)) {
        return Err(StoreError::Conflict("read only mode".to_owned()));
    }
    if rolled.is_err() {
        return Err(aborted());
    }
    Ok(())
}

async fn delete_one(
    tx: &mut Transaction<'_, Postgres>,
    id: ComponentId,
) -> Result<Recorded, StoreError> {
    let target: Option<TargetRow> = sqlx::query_as(TARGET_SQL)
        .bind(id.as_uuid())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| map_err(DELETE_CONTEXT, e))?;
    let Some(target) = target else {
        return Ok(Recorded {
            result: DeleteResult::Failed,
            item: None,
        });
    };
    let mut savepoint = tx
        .begin()
        .await
        .map_err(|e| map_err(DELETE_CONTEXT, e))?;
    let deleted = sqlx::query("DELETE FROM components WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&mut *savepoint)
        .await;
    match deleted {
        Ok(done) => {
            savepoint
                .commit()
                .await
                .map_err(|e| map_err(DELETE_CONTEXT, e))?;
            if done.rows_affected() == 0 {
                return Ok(Recorded {
                    result: DeleteResult::Failed,
                    item: None,
                });
            }
            let bytes = to_u64(target.size_bytes, DELETE_CONTEXT)?;
            Ok(Recorded {
                result: DeleteResult::Deleted { bytes },
                item: Some(kept_item(
                    &target,
                    id.as_uuid(),
                    ItemDecision::Delete,
                    "applied".to_owned(),
                )),
            })
        }
        Err(error) => {
            let mapped = map_mutation_err(DELETE_CONTEXT, error);
            rollback_savepoint(savepoint, &mapped).await?;
            Ok(verdict_of(mapped, &target, id.as_uuid()))
        }
    }
}

fn verdict_of(mapped: MutationError, target: &TargetRow, component: Uuid) -> Recorded {
    match mapped {
        MutationError::Violation(PolicyViolation::Immutable { rule }) => Recorded {
            result: DeleteResult::Skipped(ItemDecision::ProtectedImmutable),
            item: Some(kept_item(
                target,
                component,
                ItemDecision::ProtectedImmutable,
                rule.to_string(),
            )),
        },
        MutationError::Violation(PolicyViolation::Held { hold }) => Recorded {
            result: DeleteResult::Skipped(ItemDecision::ProtectedHold),
            item: Some(kept_item(
                target,
                component,
                ItemDecision::ProtectedHold,
                hold.to_string(),
            )),
        },
        MutationError::Violation(PolicyViolation::ReadOnly) => Recorded {
            result: DeleteResult::Failed,
            item: None,
        },
        _ => Recorded {
            result: DeleteResult::Failed,
            item: Some(failed_item(target, component)),
        },
    }
}

async fn insert_item(
    tx: &mut Transaction<'_, Postgres>,
    run: Uuid,
    seq: i64,
    item: &PendingItem,
    tags: &Vec<String>,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO lifecycle_run_items (run_id, seq, repository, component_id, coordinate, \
         tags, size_bytes, decision, reason) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(run)
    .bind(seq)
    .bind(&item.repository)
    .bind(item.component_id)
    .bind(&item.coordinate)
    .bind(tags)
    .bind(item.size_bytes)
    .bind(decision_text(item.decision))
    .bind(&item.reason)
    .execute(&mut **tx)
    .await
    .map_err(|e| map_err("write lifecycle item", e))?;
    Ok(())
}

#[async_trait]
impl LifecycleRepo for PgLifecycleRepo {
    async fn candidates(
        &self,
        scope: LifecycleScope,
        page: &PageRequest,
    ) -> Result<Page<Candidate>, StoreError> {
        scope_exists(&self.pool, scope).await?;
        let (repository, project) = match scope {
            LifecycleScope::Repository(id) => (Some(id.as_uuid()), None),
            LifecycleScope::Project(id) => (None, Some(id.as_uuid())),
        };
        let (limit, fetch) = cursor::fetch_limit(page);
        let (first, repo_after, group_after, name_after, at_after, id_after) = match &page.after {
            None => (
                true,
                Uuid::nil(),
                String::new(),
                String::new(),
                OffsetDateTime::UNIX_EPOCH,
                Uuid::nil(),
            ),
            Some(token) => candidate_cursor(token)?,
        };
        let rows: Vec<CandidateRow> = sqlx::query_as(CANDIDATE_SQL)
            .bind(repository)
            .bind(project)
            .bind(first)
            .bind(repo_after)
            .bind(group_after)
            .bind(name_after)
            .bind(at_after)
            .bind(id_after)
            .bind(fetch)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list lifecycle candidates", e))?;
        let rows = rows
            .into_iter()
            .map(CandidateRow::into_candidate)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(rows, limit, CANDIDATE_PAGE, |row| {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                row.repository_id,
                row.group_id.as_deref().unwrap_or(""),
                row.name,
                row.published_at.unix_timestamp_nanos(),
                row.component_id
            )
        }))
    }

    async fn apply_deletions(
        &self,
        run: LifecycleRunId,
        batch: &[ComponentId],
        options: &WriteOptions,
    ) -> Result<Vec<DeleteResult>, StoreError> {
        if batch.len() > DELETE_BATCH_LIMIT {
            return Err(StoreError::Invalid(
                "a deletion batch has at most 500 components".to_owned(),
            ));
        }
        let mut tx = write::begin(&self.pool, options, DELETE_CONTEXT)
            .await
            .map_err(MutationError::into_store_error)?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lifecycle_runs WHERE id = $1)")
            .bind(run.as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_err(DELETE_CONTEXT, e))?;
        if !exists {
            return Err(StoreError::Conflict(format!(
                "lifecycle run {run} does not exist"
            )));
        }
        let next: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM lifecycle_run_items WHERE run_id = $1",
        )
        .bind(run.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err(DELETE_CONTEXT, e))?;
        let mut seq = next;
        let mut results = Vec::with_capacity(batch.len());
        let tags = Vec::<String>::new();
        for id in batch {
            let recorded = delete_one(&mut tx, *id).await?;
            if let Some(item) = &recorded.item {
                insert_item(&mut tx, run.as_uuid(), seq, item, &tags).await?;
                seq = seq.saturating_add(1);
            }
            results.push(recorded.result);
        }
        write::commit(tx, options, DELETE_CONTEXT)
            .await
            .map_err(MutationError::into_store_error)?;
        Ok(results)
    }
}

#[async_trait]
impl LifecycleRunRepo for PgLifecycleRunRepo {
    async fn create(&self, run: &LifecycleRun) -> Result<(), StoreError> {
        let status = status_text(run.status)?;
        let counts = counts_value(&run.counts)?;
        sqlx::query(
            "INSERT INTO lifecycle_runs (id, kind, cleanup_policy_id, retention_policy_id, \
             repository_id, dry_run, policy_version, task_run_id, status, counts, truncated, \
             requested_by, confirmed_mass_delete, started_at, finished_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        )
        .bind(run.id.as_uuid())
        .bind(kind_text(run.kind))
        .bind(run.cleanup_policy_id.map(|id| id.as_uuid()))
        .bind(run.retention_policy_id.map(|id| id.as_uuid()))
        .bind(run.repository_id.map(|id| id.as_uuid()))
        .bind(run.dry_run)
        .bind(run.policy_version)
        .bind(run.task_run_id.map(|id| id.as_uuid()))
        .bind(status)
        .bind(counts)
        .bind(run.truncated)
        .bind(run.requested_by.map(|id| id.as_uuid()))
        .bind(run.confirmed_mass_delete)
        .bind(run.started_at)
        .bind(run.finished_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create lifecycle run", e))?;
        Ok(())
    }

    async fn update_progress(
        &self,
        id: LifecycleRunId,
        counts: &LifecycleCounts,
    ) -> Result<(), StoreError> {
        let counts = counts_value(counts)?;
        let done = sqlx::query("UPDATE lifecycle_runs SET counts = $2 WHERE id = $1")
            .bind(id.as_uuid())
            .bind(counts)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("update lifecycle run", e))?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("lifecycle run".to_owned()));
        }
        Ok(())
    }

    async fn finish(
        &self,
        id: LifecycleRunId,
        status: TaskRunStatus,
        counts: &LifecycleCounts,
        truncated: bool,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let counts = counts_value(counts)?;
        let done = sqlx::query(
            "UPDATE lifecycle_runs SET status = $2, counts = $3, truncated = $4, finished_at = $5 \
             WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(status_text(status)?)
        .bind(counts)
        .bind(truncated)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("finish lifecycle run", e))?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound("lifecycle run".to_owned()));
        }
        Ok(())
    }

    async fn get(&self, id: LifecycleRunId) -> Result<Option<LifecycleRun>, StoreError> {
        sqlx::query_as::<_, RunRow>(
            "SELECT id, kind, cleanup_policy_id, retention_policy_id, repository_id, dry_run,              policy_version, task_run_id, status, counts, truncated, requested_by,              confirmed_mass_delete, started_at, finished_at, created_at              FROM lifecycle_runs WHERE id = $1",
        )
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read lifecycle run", e))?
            .map(RunRow::into_run)
            .transpose()
    }

    async fn list(
        &self,
        kind: Option<LifecycleKind>,
        policy: Option<PolicyRef>,
        page: &PageRequest,
    ) -> Result<Page<LifecycleRun>, StoreError> {
        let (limit, fetch) = cursor::fetch_limit(page);
        let filter = policy_filter(policy);
        let (first, at, id) = match &page.after {
            None => (true, OffsetDateTime::UNIX_EPOCH, Uuid::nil()),
            Some(token) => run_cursor(token)?,
        };
        let rows: Vec<RunRow> = sqlx::query_as(RUN_LIST_SQL)
            .bind(kind.map(kind_text))
            .bind(filter.cleanup)
            .bind(filter.require_repository)
            .bind(filter.repository)
            .bind(filter.retention)
            .bind(first)
            .bind(at)
            .bind(id)
            .bind(fetch)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list lifecycle runs", e))?;
        let mut stamped: Vec<(OffsetDateTime, LifecycleRun)> = rows
            .into_iter()
            .map(|row| {
                let at = row.created_at;
                row.into_run().map(|run| (at, run))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next = if stamped.len() > limit {
            stamped.truncate(limit);
            stamped.last().map(|(at, run)| {
                cursor::encode(RUN_PAGE, &format!("{}|{}", at.unix_timestamp_nanos(), run.id))
            })
        } else {
            None
        };
        Ok(Page {
            items: stamped.into_iter().map(|(_, run)| run).collect(),
            next,
        })
    }

    async fn save_items(
        &self,
        run: LifecycleRunId,
        items: &[LifecycleItem],
    ) -> Result<bool, StoreError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lifecycle_runs WHERE id = $1)")
                .bind(run.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("write lifecycle items", e))?;
        if !exists {
            return Err(StoreError::NotFound("lifecycle run".to_owned()));
        }
        let mut seen = HashSet::with_capacity(items.len());
        let mut seqs = Vec::with_capacity(items.len());
        for item in items {
            let seq = to_i64(item.seq)?;
            if !seen.insert(seq) {
                return Err(StoreError::Conflict(format!(
                    "item {} of the run already exists",
                    item.seq
                )));
            }
            seqs.push(seq);
        }
        let clash: Option<i64> = sqlx::query_scalar(
            "SELECT seq FROM lifecycle_run_items WHERE run_id = $1 AND seq = ANY($2) LIMIT 1",
        )
        .bind(run.as_uuid())
        .bind(&seqs)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("write lifecycle items", e))?;
        if let Some(seq) = clash {
            return Err(StoreError::Conflict(format!(
                "item {seq} of the run already exists"
            )));
        }
        let stored: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM lifecycle_run_items WHERE run_id = $1",
        )
        .bind(run.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("write lifecycle items", e))?;
        let cap = i64::try_from(MAX_LIFECYCLE_ITEMS_PER_RUN).unwrap_or(i64::MAX);
        let mut kept = stored;
        let mut cut = false;
        for item in items {
            if kept >= cap {
                cut = true;
                continue;
            }
            let seq = to_i64(item.seq)?;
            let size = to_i64(item.size_bytes)?;
            sqlx::query(
                "INSERT INTO lifecycle_run_items (run_id, seq, repository, component_id, \
                 coordinate, tags, size_bytes, decision, reason) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(run.as_uuid())
            .bind(seq)
            .bind(&item.repository)
            .bind(item.component_id.as_uuid())
            .bind(&item.coordinate)
            .bind(&item.tags)
            .bind(size)
            .bind(decision_text(item.decision))
            .bind(&item.reason)
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("write lifecycle items", e))?;
            kept = kept.saturating_add(1);
        }
        Ok(cut)
    }

    async fn list_items(
        &self,
        run: LifecycleRunId,
        page: &PageRequest,
    ) -> Result<Page<LifecycleItem>, StoreError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lifecycle_runs WHERE id = $1)")
                .bind(run.as_uuid())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| map_err("list lifecycle items", e))?;
        if !exists {
            return Err(StoreError::NotFound("lifecycle run".to_owned()));
        }
        let (limit, fetch) = cursor::fetch_limit(page);
        let after = match &page.after {
            None => None,
            Some(token) => Some(
                cursor::decode(ITEM_PAGE, token)?
                    .parse::<i64>()
                    .map_err(|_| cursor::invalid_token())?,
            ),
        };
        let rows: Vec<ItemRow> = sqlx::query_as(
            "SELECT run_id, seq, repository, component_id, coordinate, tags, size_bytes, \
             decision, reason FROM lifecycle_run_items \
             WHERE run_id = $1 AND ($2::bigint IS NULL OR seq > $2) ORDER BY seq LIMIT $3",
        )
        .bind(run.as_uuid())
        .bind(after)
        .bind(fetch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_err("list lifecycle items", e))?;
        let rows = rows
            .into_iter()
            .map(ItemRow::into_item)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cursor::finish(rows, limit, ITEM_PAGE, |item| {
            item.seq.to_string()
        }))
    }

    fn stream_items(
        &self,
        run: LifecycleRunId,
    ) -> BoxStream<'static, Result<LifecycleItem, StoreError>> {
        let pool = self.pool.clone();
        let run_id = run.as_uuid();
        stream::unfold(Some((pool, run_id, -1_i64)), |state| async move {
            let (pool, run_id, after) = state?;
            let rows: Result<Vec<ItemRow>, sqlx::Error> = sqlx::query_as(
                "SELECT run_id, seq, repository, component_id, coordinate, tags, size_bytes, \
                 decision, reason FROM lifecycle_run_items \
                 WHERE run_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
            )
            .bind(run_id)
            .bind(after)
            .bind(STREAM_PAGE)
            .fetch_all(&pool)
            .await;
            let rows = match rows {
                Ok(rows) => rows,
                Err(error) => {
                    return Some((
                        vec![Err(map_err("list lifecycle items", error))],
                        None,
                    ));
                }
            };
            if rows.is_empty() {
                return None;
            }
            let next = rows.last().map(|row| row.seq).unwrap_or(after);
            let items = rows.into_iter().map(ItemRow::into_item).collect();
            Some((items, Some((pool, run_id, next))))
        })
        .flat_map(stream::iter)
        .boxed()
    }

    async fn last_dry_run(
        &self,
        policy: PolicyRef,
        policy_version: i64,
        max_age: Duration,
    ) -> Result<Option<LifecycleRun>, StoreError> {
        let age = time::Duration::try_from(max_age).map_err(|_| {
            StoreError::Invalid("max age out of range".to_owned())
        })?;
        let cutoff = OffsetDateTime::now_utc()
            .checked_sub(age)
            .ok_or_else(|| StoreError::Invalid("max age out of range".to_owned()))?;
        let filter = policy_filter(Some(policy));
        sqlx::query_as::<_, RunRow>(DRY_SQL)
            .bind(policy_version)
            .bind(cutoff)
            .bind(filter.cleanup)
            .bind(filter.require_repository)
            .bind(filter.repository)
            .bind(filter.retention)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read lifecycle run", e))?
            .map(RunRow::into_run)
            .transpose()
    }

    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError> {
        let done = sqlx::query(
            "DELETE FROM lifecycle_runs WHERE finished_at IS NOT NULL AND finished_at < $1",
        )
        .bind(finished_before)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("purge lifecycle runs", e))?;
        Ok(done.rows_affected())
    }
}
