//! Replication rules. Executions and tasks are not stored yet.

use std::time::Duration;

use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventDraft};
use rustryx_core::ids::{
    ProjectId, RegistryId, ReplicationExecutionId, ReplicationRuleId, ReplicationTaskId,
    RepositoryId, TaskDefinitionId, UserId,
};
use rustryx_core::ports::{Page, PageRequest, ReplicationRepo};
use rustryx_core::replication::{
    ReplicationDirection, ReplicationExecution, ReplicationFilter, ReplicationRule,
    ReplicationScope, ReplicationTask, ReplicationTaskStatus, ReplicationTrigger,
};
use rustryx_core::tasks::TaskLogLine;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{backend, map_err};

#[path = "replication_exec.rs"]
mod exec;

impl PgReplicationRepo {
    pub(super) fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Replication rules.
#[derive(Clone)]
pub struct PgReplicationRepo {
    pool: PgPool,
}

impl PgReplicationRepo {
    /// A repo on `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    direction: String,
    registry_id: Uuid,
    repository_id: Option<Uuid>,
    project_id: Option<Uuid>,
    remote_namespace: String,
    flatten_levels: i16,
    override_existing: bool,
    filter: serde_json::Value,
    trigger: String,
    on_push: bool,
    on_delete: bool,
    schedule_task_id: Option<Uuid>,
    copy_referrers: bool,
    replicate_deletion: bool,
    bandwidth_limit_kbps: Option<i32>,
    event_settle_secs: i32,
    enabled: bool,
    version: i64,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[async_trait::async_trait]
impl ReplicationRepo for PgReplicationRepo {
    async fn create_rule(&self, rule: &ReplicationRule) -> Result<(), StoreError> {
        rule.validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let (direction, trigger, on_push, on_delete, scope_repo, scope_project, filter, bandwidth) =
            columns(rule)?;
        sqlx::query(
            "INSERT INTO replication_rules (id, name, direction, registry_id, repository_id, \
             project_id, remote_namespace, flatten_levels, override_existing, filter, trigger, \
             on_push, on_delete, schedule_task_id, copy_referrers, replicate_deletion, \
             bandwidth_limit_kbps, event_settle_secs, enabled, version, created_by, created_at, \
             updated_at) VALUES \
             ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)",
        )
        .bind(rule.id.as_uuid())
        .bind(&rule.name)
        .bind(direction)
        .bind(rule.registry_id.as_uuid())
        .bind(scope_repo)
        .bind(scope_project)
        .bind(&rule.remote_namespace)
        .bind(i16::from(rule.flatten_levels))
        .bind(rule.override_existing)
        .bind(filter)
        .bind(trigger)
        .bind(on_push)
        .bind(on_delete)
        .bind(rule.schedule_task_id.map(|id| id.as_uuid()))
        .bind(rule.copy_referrers)
        .bind(rule.replicate_deletion)
        .bind(bandwidth)
        .bind(i32::try_from(rule.event_settle_secs).unwrap_or(i32::MAX))
        .bind(rule.enabled)
        .bind(rule.version)
        .bind(rule.created_by.map(|id| id.as_uuid()))
        .bind(rule.created_at)
        .bind(rule.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("create replication rule", e))?;
        Ok(())
    }

    async fn get_rule(&self, id: ReplicationRuleId) -> Result<Option<ReplicationRule>, StoreError> {
        let row: Option<Row> = sqlx::query_as("SELECT * FROM replication_rules WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_err("read replication rule", e))?;
        row.map(decode).transpose()
    }

    async fn list_rules(&self) -> Result<Vec<ReplicationRule>, StoreError> {
        let rows: Vec<Row> = sqlx::query_as("SELECT * FROM replication_rules ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list replication rules", e))?;
        rows.into_iter().map(decode).collect()
    }

    async fn update_rule(
        &self,
        rule: &ReplicationRule,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        rule.validate()
            .map_err(|error| StoreError::Conflict(error.to_string()))?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT version FROM replication_rules WHERE id = $1")
                .bind(rule.id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| map_err("read replication rule", e))?;
        let Some(current) = current else {
            return Err(StoreError::NotFound("replication rule".to_owned()));
        };
        if current != expected_version {
            return Err(StoreError::Conflict(format!(
                "stale version: expected {expected_version}, found {current}"
            )));
        }
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM replication_executions \
             WHERE rule_id = $1 AND status IN ('queued', 'running'))",
        )
        .bind(rule.id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("read replication execution", e))?;
        if active {
            return Err(StoreError::Conflict(
                "the rule has an execution that is queued or running".to_owned(),
            ));
        }
        let (direction, trigger, on_push, on_delete, scope_repo, scope_project, filter, bandwidth) =
            columns(rule)?;
        sqlx::query(
            "UPDATE replication_rules SET name = $2, direction = $3, registry_id = $4, \
             repository_id = $5, project_id = $6, remote_namespace = $7, flatten_levels = $8, \
             override_existing = $9, filter = $10, trigger = $11, on_push = $12, on_delete = $13, \
             schedule_task_id = $14, copy_referrers = $15, replicate_deletion = $16, \
             bandwidth_limit_kbps = $17, event_settle_secs = $18, enabled = $19, \
             version = $20, updated_at = $21 WHERE id = $1 AND version = $22",
        )
        .bind(rule.id.as_uuid())
        .bind(&rule.name)
        .bind(direction)
        .bind(rule.registry_id.as_uuid())
        .bind(scope_repo)
        .bind(scope_project)
        .bind(&rule.remote_namespace)
        .bind(i16::from(rule.flatten_levels))
        .bind(rule.override_existing)
        .bind(filter)
        .bind(trigger)
        .bind(on_push)
        .bind(on_delete)
        .bind(rule.schedule_task_id.map(|id| id.as_uuid()))
        .bind(rule.copy_referrers)
        .bind(rule.replicate_deletion)
        .bind(bandwidth)
        .bind(i32::try_from(rule.event_settle_secs).unwrap_or(i32::MAX))
        .bind(rule.enabled)
        .bind(expected_version + 1)
        .bind(rule.updated_at)
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(|e| map_err("update replication rule", e))?;
        Ok(())
    }

    async fn delete_rule(&self, id: ReplicationRuleId) -> Result<(), StoreError> {
        let deleted = sqlx::query("DELETE FROM replication_rules WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| map_err("delete replication rule", e))?;
        if deleted.rows_affected() == 0 {
            return Err(StoreError::NotFound("replication rule".to_owned()));
        }
        Ok(())
    }

    async fn start_execution(
        &self,
        execution: &ReplicationExecution,
    ) -> Result<Option<ReplicationExecution>, StoreError> {
        exec::start_execution(self, execution).await
    }

    async fn get_execution(
        &self,
        id: ReplicationExecutionId,
    ) -> Result<Option<ReplicationExecution>, StoreError> {
        exec::get_execution(self, id).await
    }

    async fn list_executions(
        &self,
        rule: Option<ReplicationRuleId>,
        page: &PageRequest,
    ) -> Result<Page<ReplicationExecution>, StoreError> {
        exec::list_executions(self, rule, page).await
    }

    async fn request_stop(&self, id: ReplicationExecutionId) -> Result<bool, StoreError> {
        exec::request_stop(self, id).await
    }

    async fn add_tasks(&self, tasks: &[ReplicationTask]) -> Result<(), StoreError> {
        exec::add_tasks(self, tasks).await
    }

    async fn create_event_tasks(
        &self,
        rule: ReplicationRuleId,
        tasks: &[ReplicationTask],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError> {
        exec::create_event_tasks(self, rule, tasks, advance).await
    }

    async fn claim_task(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<ReplicationTask>, StoreError> {
        exec::claim_task(self, owner, lease).await
    }

    async fn heartbeat_task(
        &self,
        id: ReplicationTaskId,
        owner: &str,
        lease: Duration,
    ) -> Result<bool, StoreError> {
        exec::heartbeat_task(self, id, owner, lease).await
    }

    async fn finish_task(
        &self,
        id: ReplicationTaskId,
        owner: &str,
        status: ReplicationTaskStatus,
        bytes: u64,
        error: Option<&str>,
        retry_in: Option<Duration>,
        events: &[EventDraft],
    ) -> Result<bool, StoreError> {
        exec::finish_task(self, id, owner, status, bytes, error, retry_in, events).await
    }

    async fn tasks(
        &self,
        execution: ReplicationExecutionId,
        page: &PageRequest,
    ) -> Result<Page<ReplicationTask>, StoreError> {
        exec::tasks(self, execution, page).await
    }

    async fn append_task_log(
        &self,
        id: ReplicationTaskId,
        lines: &[TaskLogLine],
    ) -> Result<(), StoreError> {
        exec::append_task_log(self, id, lines).await
    }

    async fn read_task_log(
        &self,
        id: ReplicationTaskId,
        after_seq: i32,
        limit: u32,
    ) -> Result<Vec<TaskLogLine>, StoreError> {
        exec::read_task_log(self, id, after_seq, limit).await
    }

    async fn settle_executions(&self, now: OffsetDateTime) -> Result<u64, StoreError> {
        exec::settle_executions(self, now).await
    }

    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError> {
        exec::purge(self, finished_before).await
    }
}

fn columns(
    rule: &ReplicationRule,
) -> Result<
    (
        &'static str,
        &'static str,
        bool,
        bool,
        Option<Uuid>,
        Option<Uuid>,
        serde_json::Value,
        Option<i32>,
    ),
    StoreError,
> {
    let direction = match rule.direction {
        ReplicationDirection::Push => "push",
        ReplicationDirection::Pull => "pull",
    };
    let (trigger, on_push, on_delete) = match rule.trigger {
        ReplicationTrigger::Manual => ("manual", false, false),
        ReplicationTrigger::Scheduled => ("scheduled", false, false),
        ReplicationTrigger::Event { on_push, on_delete } => ("event", on_push, on_delete),
    };
    let (repository, project) = match rule.local {
        ReplicationScope::Repository(id) => (Some(id.as_uuid()), None),
        ReplicationScope::Project(id) => (None, Some(id.as_uuid())),
    };
    let filter = serde_json::to_value(&rule.filter)
        .map_err(|_| backend("write replication rule", "filter does not serialize"))?;
    let bandwidth = match rule.bandwidth_limit_kbps {
        None => None,
        Some(limit) => Some(
            i32::try_from(limit)
                .map_err(|_| StoreError::Invalid("bandwidth limit is out of range".to_owned()))?,
        ),
    };
    Ok((
        direction, trigger, on_push, on_delete, repository, project, filter, bandwidth,
    ))
}

fn decode(row: Row) -> Result<ReplicationRule, StoreError> {
    let direction = match row.direction.as_str() {
        "push" => ReplicationDirection::Push,
        "pull" => ReplicationDirection::Pull,
        _ => return Err(backend("read replication rule", "direction is unknown")),
    };
    let trigger = match row.trigger.as_str() {
        "manual" => ReplicationTrigger::Manual,
        "scheduled" => ReplicationTrigger::Scheduled,
        "event" => ReplicationTrigger::Event {
            on_push: row.on_push,
            on_delete: row.on_delete,
        },
        _ => return Err(backend("read replication rule", "trigger is unknown")),
    };
    let local = match (row.repository_id, row.project_id) {
        (Some(id), None) => ReplicationScope::Repository(RepositoryId::from_uuid(id)),
        (None, Some(id)) => ReplicationScope::Project(ProjectId::from_uuid(id)),
        _ => return Err(backend("read replication rule", "scope is ambiguous")),
    };
    let filter: ReplicationFilter = serde_json::from_value(row.filter)
        .map_err(|_| backend("read replication rule", "stored filter is not a filter"))?;
    let flatten = u8::try_from(row.flatten_levels)
        .map_err(|_| backend("read replication rule", "flatten levels are out of range"))?;
    let settle = u32::try_from(row.event_settle_secs)
        .map_err(|_| backend("read replication rule", "settle time is out of range"))?;
    let bandwidth = row
        .bandwidth_limit_kbps
        .map(|limit| {
            u32::try_from(limit)
                .map_err(|_| backend("read replication rule", "bandwidth limit is out of range"))
        })
        .transpose()?;
    let mut rule = ReplicationRule::new(
        ReplicationRuleId::from_uuid(row.id),
        row.name,
        direction,
        RegistryId::from_uuid(row.registry_id),
        local,
        row.created_at,
    );
    rule.remote_namespace = row.remote_namespace;
    rule.flatten_levels = flatten;
    rule.override_existing = row.override_existing;
    rule.filter = filter;
    rule.trigger = trigger;
    rule.schedule_task_id = row.schedule_task_id.map(TaskDefinitionId::from_uuid);
    rule.copy_referrers = row.copy_referrers;
    rule.replicate_deletion = row.replicate_deletion;
    rule.bandwidth_limit_kbps = bandwidth;
    rule.event_settle_secs = settle;
    rule.enabled = row.enabled;
    rule.version = row.version;
    rule.created_by = row.created_by.map(UserId::from_uuid);
    rule.updated_at = row.updated_at;
    Ok(rule)
}
