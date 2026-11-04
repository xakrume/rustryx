//! Executions and tasks of a replication rule.

use std::time::Duration;

use rustryx_core::audit::ActorRef;
use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventDraft, EventKind};
use rustryx_core::ids::{ReplicationExecutionId, ReplicationRuleId, ReplicationTaskId, UserId};
use rustryx_core::ports::{Page, PageRequest};
use rustryx_core::replication::{
    ExecutionCounts, ExecutionStatus, ReplicationExecution, ReplicationTask, ReplicationTaskKind,
    ReplicationTaskStatus,
};
use rustryx_core::tasks::{TaskLogLine, TaskTrigger};
use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

use super::super::common::to_i64;
use super::PgReplicationRepo;
use crate::error::{backend, map_err};
use crate::outbox;

const MAX_TASKS: usize = 1000;
const GRACE_SECS: i64 = 60;

#[derive(sqlx::FromRow)]
struct ExecRow {
    id: Uuid,
    rule_id: Uuid,
    trigger: String,
    status: String,
    stop_requested_at: Option<OffsetDateTime>,
    requested_by: Option<Uuid>,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct TaskRow {
    id: Uuid,
    execution_id: Uuid,
    kind: String,
    repository: String,
    image: String,
    reference: String,
    status: String,
    attempts: i16,
    bytes_transferred: i64,
    error: Option<String>,
    available_at: OffsetDateTime,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
}

pub(super) async fn start_execution(
    repo: &PgReplicationRepo,
    execution: &ReplicationExecution,
) -> Result<Option<ReplicationExecution>, StoreError> {
    let inserted = sqlx::query(
        "INSERT INTO replication_executions (id, rule_id, trigger, status, counts, \
         stop_requested_at, requested_by, started_at, finished_at, created_at) \
         SELECT $1, $2, $3, $4, '{}'::jsonb, $5, $6, $7, $8, $9 \
         WHERE EXISTS (SELECT 1 FROM replication_rules WHERE id = $2) \
           AND NOT EXISTS (SELECT 1 FROM replication_executions \
                           WHERE rule_id = $2 AND status IN ('queued', 'running'))",
    )
    .bind(execution.id.as_uuid())
    .bind(execution.rule_id.as_uuid())
    .bind(trigger_label(execution.trigger)?)
    .bind(status_label(execution.status)?)
    .bind(execution.stop_requested.then_some(execution.created_at))
    .bind(execution.requested_by.map(|id| id.as_uuid()))
    .bind(execution.started_at)
    .bind(execution.finished_at)
    .bind(execution.created_at)
    .execute(repo.pool())
    .await
    .map_err(|e| map_err("start replication execution", e))?;
    if inserted.rows_affected() == 1 {
        let mut stored = execution.clone();
        stored.counts = ExecutionCounts::default();
        return Ok(Some(stored));
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM replication_rules WHERE id = $1)")
        .bind(execution.rule_id.as_uuid())
        .fetch_one(repo.pool())
        .await
        .map_err(|e| map_err("read replication rule", e))?;
    if !exists {
        return Err(StoreError::Conflict(format!(
            "rule {} does not exist",
            execution.rule_id
        )));
    }
    Ok(None)
}

pub(super) async fn get_execution(
    repo: &PgReplicationRepo,
    id: ReplicationExecutionId,
) -> Result<Option<ReplicationExecution>, StoreError> {
    let row: Option<ExecRow> = sqlx::query_as("SELECT * FROM replication_executions WHERE id = $1")
        .bind(id.as_uuid())
        .fetch_optional(repo.pool())
        .await
        .map_err(|e| map_err("read replication execution", e))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let mut execution = decode_exec(row)?;
    execution.counts = counts(repo.pool(), id).await?;
    Ok(Some(execution))
}

pub(super) async fn list_executions(
    repo: &PgReplicationRepo,
    rule: Option<ReplicationRuleId>,
    page: &PageRequest,
) -> Result<Page<ReplicationExecution>, StoreError> {
    let limit = page.limit.clamp(1, 100);
    let after = match page.after.as_deref() {
        None => None,
        Some(token) => Some(
            Uuid::parse_str(token)
                .map_err(|_| StoreError::Invalid("page token is not an execution id".to_owned()))?,
        ),
    };
    let rows: Vec<ExecRow> = sqlx::query_as(
        "SELECT * FROM replication_executions \
         WHERE ($1::uuid IS NULL OR rule_id = $1) \
           AND ($2::uuid IS NULL OR (created_at, id) < ( \
                SELECT created_at, id FROM replication_executions WHERE id = $2)) \
         ORDER BY created_at DESC, id DESC LIMIT $3",
    )
    .bind(rule.map(|id| id.as_uuid()))
    .bind(after)
    .bind(i64::from(limit + 1))
    .fetch_all(repo.pool())
    .await
    .map_err(|e| map_err("list replication executions", e))?;
    let more = rows.len() > limit as usize;
    let mut items = Vec::new();
    for row in rows.into_iter().take(limit as usize) {
        let id = ReplicationExecutionId::from_uuid(row.id);
        let mut execution = decode_exec(row)?;
        execution.counts = counts(repo.pool(), id).await?;
        items.push(execution);
    }
    let next = more
        .then(|| items.last().map(|item| item.id.to_string()))
        .flatten();
    Ok(Page { items, next })
}

pub(super) async fn request_stop(
    repo: &PgReplicationRepo,
    id: ReplicationExecutionId,
) -> Result<bool, StoreError> {
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM replication_executions WHERE id = $1")
            .bind(id.as_uuid())
            .fetch_optional(repo.pool())
            .await
            .map_err(|e| map_err("read replication execution", e))?;
    let Some(status) = status else {
        return Err(StoreError::NotFound("replication execution".to_owned()));
    };
    if matches!(status.as_str(), "succeeded" | "partially_failed" | "failed" | "stopped") {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE replication_executions SET stop_requested_at = COALESCE(stop_requested_at, now()) \
         WHERE id = $1",
    )
    .bind(id.as_uuid())
    .execute(repo.pool())
    .await
    .map_err(|e| map_err("stop replication execution", e))?;
    Ok(true)
}

pub(super) async fn add_tasks(
    repo: &PgReplicationRepo,
    tasks: &[ReplicationTask],
) -> Result<(), StoreError> {
    if tasks.len() > MAX_TASKS {
        return Err(StoreError::Invalid("at most 1000 tasks per call".to_owned()));
    }
    let mut tx = repo.pool().begin().await.map_err(|e| map_err("add replication tasks", e))?;
    for task in tasks {
        insert_task(&mut tx, task).await?;
    }
    tx.commit().await.map_err(|e| map_err("add replication tasks", e))
}

pub(super) async fn create_event_tasks(
    repo: &PgReplicationRepo,
    rule: ReplicationRuleId,
    tasks: &[ReplicationTask],
    advance: Option<&CursorAdvance>,
) -> Result<u64, StoreError> {
    if tasks.len() > MAX_TASKS {
        return Err(StoreError::Invalid("at most 1000 tasks per call".to_owned()));
    }
    let mut tx = repo.pool().begin().await.map_err(|e| map_err("create event tasks", e))?;
    if let Some(advance) = advance
        && !outbox::advance_cursor(&mut tx, advance).await?
    {
        return Ok(0);
    }
    let rule_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM replication_rules WHERE id = $1)")
        .bind(rule.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err("read replication rule", e))?;
    if !rule_exists {
        return Err(StoreError::NotFound("replication rule".to_owned()));
    }
    let open: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM replication_executions \
         WHERE rule_id = $1 AND status IN ('queued', 'running') LIMIT 1",
    )
    .bind(rule.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| map_err("read replication execution", e))?;
    let execution = if let Some(id) = open {
        id
    } else {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO replication_executions (id, rule_id, trigger, status, created_at) \
             VALUES ($1, $2, 'event', 'queued', now())",
        )
        .bind(id)
        .bind(rule.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("start replication execution", e))?;
        id
    };
    for task in tasks {
        let mut row = task.clone();
        row.execution_id = ReplicationExecutionId::from_uuid(execution);
        insert_task(&mut tx, &row).await?;
    }
    tx.commit().await.map_err(|e| map_err("create event tasks", e))?;
    Ok(u64::try_from(tasks.len()).unwrap_or(u64::MAX))
}

pub(super) async fn claim_task(
    repo: &PgReplicationRepo,
    owner: &str,
    lease: Duration,
) -> Result<Option<ReplicationTask>, StoreError> {
    let seconds = i32::try_from(lease.as_secs()).unwrap_or(i32::MAX);
    let row: Option<TaskRow> = sqlx::query_as(
        "WITH candidate AS ( \
            SELECT t.id FROM replication_tasks t \
            JOIN replication_executions e ON e.id = t.execution_id \
            WHERE e.status IN ('queued', 'running') AND e.stop_requested_at IS NULL \
              AND ((t.status = 'pending' AND t.available_at <= now()) \
                   OR (t.status = 'running' AND t.lease_expires_at <= now())) \
            ORDER BY t.available_at, t.id \
            FOR UPDATE OF t SKIP LOCKED LIMIT 1), \
         claimed AS ( \
            UPDATE replication_tasks SET status = 'running', attempts = attempts + 1, \
                started_at = COALESCE(started_at, now()), lease_owner = $1, \
                lease_expires_at = now() + make_interval(secs => $2) \
            WHERE id = (SELECT id FROM candidate) RETURNING *), \
         promoted AS ( \
            UPDATE replication_executions SET status = 'running', \
                started_at = COALESCE(started_at, now()) \
            WHERE id = (SELECT execution_id FROM claimed) AND status = 'queued') \
         SELECT id, execution_id, kind, repository, image, reference, status, attempts, \
                bytes_transferred, error, available_at, started_at, finished_at FROM claimed",
    )
    .bind(owner)
    .bind(seconds)
    .fetch_optional(repo.pool())
    .await
    .map_err(|e| map_err("claim replication task", e))?;
    row.map(decode_task).transpose()
}

pub(super) async fn heartbeat_task(
    repo: &PgReplicationRepo,
    id: ReplicationTaskId,
    owner: &str,
    lease: Duration,
) -> Result<bool, StoreError> {
    let seconds = i32::try_from(lease.as_secs()).unwrap_or(i32::MAX);
    let updated = sqlx::query(
        "UPDATE replication_tasks SET lease_expires_at = now() + make_interval(secs => $3) \
         WHERE id = $1 AND lease_owner = $2 AND status = 'running'",
    )
    .bind(id.as_uuid())
    .bind(owner)
    .bind(seconds)
    .execute(repo.pool())
    .await
    .map_err(|e| map_err("heartbeat replication task", e))?;
    Ok(updated.rows_affected() == 1)
}

pub(super) async fn finish_task(
    repo: &PgReplicationRepo,
    id: ReplicationTaskId,
    owner: &str,
    status: ReplicationTaskStatus,
    bytes: u64,
    error: Option<&str>,
    retry_in: Option<Duration>,
    events: &[EventDraft],
) -> Result<bool, StoreError> {
    if retry_in.is_none() && !status.is_terminal() {
        return Err(StoreError::Invalid(
            "a task ends with a terminal status or goes back with a retry delay".to_owned(),
        ));
    }
    let mut tx = repo.pool().begin().await.map_err(|e| map_err("finish replication task", e))?;
    let held: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM replication_tasks \
         WHERE id = $1 AND lease_owner = $2 AND status = 'running' FOR UPDATE",
    )
    .bind(id.as_uuid())
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| map_err("read replication task", e))?;
    if held.is_none() {
        return Ok(false);
    }
    let code = error.map(|text| text.chars().take(64).collect::<String>());
    let bytes = to_i64(bytes)?;
    if let Some(delay) = retry_in {
        let seconds = i32::try_from(delay.as_secs()).unwrap_or(i32::MAX);
        sqlx::query(
            "UPDATE replication_tasks SET status = 'pending', bytes_transferred = $2, error = $3, \
             lease_owner = NULL, lease_expires_at = NULL, finished_at = NULL, \
             available_at = now() + make_interval(secs => $4) WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(bytes)
        .bind(code)
        .bind(seconds)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("finish replication task", e))?;
    } else {
        sqlx::query(
            "UPDATE replication_tasks SET status = $2, bytes_transferred = $3, error = $4, \
             lease_owner = NULL, lease_expires_at = NULL, finished_at = now() WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(task_status_label(status)?)
        .bind(bytes)
        .bind(code)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("finish replication task", e))?;
    }
    outbox::insert_events(&mut tx, events).await?;
    tx.commit().await.map_err(|e| map_err("finish replication task", e))?;
    Ok(true)
}

pub(super) async fn tasks(
    repo: &PgReplicationRepo,
    execution: ReplicationExecutionId,
    page: &PageRequest,
) -> Result<Page<ReplicationTask>, StoreError> {
    let limit = page.limit.clamp(1, 100);
    let after = match page.after.as_deref() {
        None => None,
        Some(token) => Some(
            Uuid::parse_str(token)
                .map_err(|_| StoreError::Invalid("page token is not a task id".to_owned()))?,
        ),
    };
    let rows: Vec<TaskRow> = sqlx::query_as(
        "SELECT id, execution_id, kind, repository, image, reference, status, attempts, \
         bytes_transferred, error, available_at, started_at, finished_at \
         FROM replication_tasks WHERE execution_id = $1 AND ($2::uuid IS NULL OR id > $2) \
         ORDER BY id LIMIT $3",
    )
    .bind(execution.as_uuid())
    .bind(after)
    .bind(i64::from(limit))
    .fetch_all(repo.pool())
    .await
    .map_err(|e| map_err("list replication tasks", e))?;
    let items = rows.into_iter().map(decode_task).collect::<Result<Vec<_>, _>>()?;
    let next = (items.len() == limit as usize).then(|| items.last().map(|task| task.id.to_string())).flatten();
    Ok(Page { items, next })
}

pub(super) async fn append_task_log(
    repo: &PgReplicationRepo,
    id: ReplicationTaskId,
    lines: &[TaskLogLine],
) -> Result<(), StoreError> {
    let mut tx = repo.pool().begin().await.map_err(|e| map_err("append replication log", e))?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM replication_tasks WHERE id = $1)")
        .bind(id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err("read replication task", e))?;
    if !exists {
        return Err(StoreError::NotFound("replication task".to_owned()));
    }
    for line in lines {
        let message: String = line.message.chars().take(1024).collect();
        sqlx::query(
            "INSERT INTO replication_task_log (task_id, seq, at, level, message) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(id.as_uuid())
        .bind(line.seq)
        .bind(line.at)
        .bind(i16::from(line.level))
        .bind(message)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("append replication log", e))?;
    }
    tx.commit().await.map_err(|e| map_err("append replication log", e))
}

pub(super) async fn read_task_log(
    repo: &PgReplicationRepo,
    id: ReplicationTaskId,
    after_seq: i32,
    limit: u32,
) -> Result<Vec<TaskLogLine>, StoreError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM replication_tasks WHERE id = $1)")
        .bind(id.as_uuid())
        .fetch_one(repo.pool())
        .await
        .map_err(|e| map_err("read replication task", e))?;
    if !exists {
        return Err(StoreError::NotFound("replication task".to_owned()));
    }
    let rows: Vec<(i32, OffsetDateTime, i16, String)> = sqlx::query_as(
        "SELECT seq, at, level, message FROM replication_task_log \
         WHERE task_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
    )
    .bind(id.as_uuid())
    .bind(after_seq)
    .bind(i64::from(limit.clamp(1, 1000)))
    .fetch_all(repo.pool())
    .await
    .map_err(|e| map_err("read replication log", e))?;
    Ok(rows
        .into_iter()
        .map(|(seq, at, level, message)| TaskLogLine {
            seq,
            at,
            level: u8::try_from(level).unwrap_or(u8::MAX),
            message,
        })
        .collect())
}

pub(super) async fn settle_executions(
    repo: &PgReplicationRepo,
    now: OffsetDateTime,
) -> Result<u64, StoreError> {
    let mut tx = repo.pool().begin().await.map_err(|e| map_err("settle replication", e))?;
    let open: Vec<(Uuid, Uuid, bool, OffsetDateTime)> = sqlx::query_as(
        "SELECT id, rule_id, stop_requested_at IS NOT NULL, created_at \
         FROM replication_executions WHERE status IN ('queued', 'running') FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| map_err("read replication execution", e))?;
    let mut settled = 0_u64;
    for (id, rule_id, stop, created_at) in open {
        if stop {
            sqlx::query(
                "UPDATE replication_tasks SET status = 'stopped', finished_at = $2 \
                 WHERE execution_id = $1 AND status = 'pending'",
            )
            .bind(id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("stop replication tasks", e))?;
        }
        let (total, succeeded, failed, skipped, in_progress) = counts_raw(&mut tx, id).await?;
        let age = now - created_at;
        let young = total == 0 && age.whole_seconds() < GRACE_SECS;
        if in_progress > 0 || young {
            continue;
        }
        let status = if stop {
            "stopped"
        } else if failed == 0 {
            "succeeded"
        } else if succeeded + skipped == 0 {
            "failed"
        } else {
            "partially_failed"
        };
        sqlx::query(
            "UPDATE replication_executions SET status = $2, finished_at = $3 WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_err("settle replication", e))?;
        let draft = EventDraft::new(
            EventKind::ReplicationFinished,
            ActorRef::system("replication"),
            serde_json::json!({
                "execution_id": id,
                "rule_id": rule_id,
                "status": status,
                "total": total,
                "succeeded": succeeded,
                "failed": failed,
                "skipped": skipped,
            }),
        );
        outbox::insert_events(&mut tx, &[draft]).await?;
        settled += 1;
    }
    tx.commit().await.map_err(|e| map_err("settle replication", e))?;
    Ok(settled)
}

pub(super) async fn purge(
    repo: &PgReplicationRepo,
    finished_before: OffsetDateTime,
) -> Result<u64, StoreError> {
    let deleted = sqlx::query("DELETE FROM replication_executions WHERE finished_at < $1")
        .bind(finished_before)
        .execute(repo.pool())
        .await
        .map_err(|e| map_err("purge replication executions", e))?;
    Ok(deleted.rows_affected())
}

async fn insert_task(tx: &mut PgConnection, task: &ReplicationTask) -> Result<(), StoreError> {
    task.validate()
        .map_err(|error| StoreError::Conflict(error.to_string()))?;
    sqlx::query(
        "INSERT INTO replication_tasks (id, execution_id, kind, repository, image, reference, \
         status, attempts, bytes_transferred, error, available_at, started_at, finished_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(task.id.as_uuid())
    .bind(task.execution_id.as_uuid())
    .bind(kind_label(task.kind)?)
    .bind(&task.repository)
    .bind(&task.image)
    .bind(&task.reference)
    .bind(task_status_label(task.status)?)
    .bind(i16::from(task.attempts))
    .bind(to_i64(task.bytes_transferred)?)
    .bind(&task.error)
    .bind(task.available_at)
    .bind(task.started_at)
    .bind(task.finished_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| map_err("add replication task", e))?;
    Ok(())
}

async fn counts(pool: &sqlx::PgPool, id: ReplicationExecutionId) -> Result<ExecutionCounts, StoreError> {
    let mut conn = pool.acquire().await.map_err(|e| map_err("count replication tasks", e))?;
    let (total, succeeded, failed, skipped, in_progress) =
        counts_raw(&mut conn, id.as_uuid()).await?;
    Ok(ExecutionCounts { total, succeeded, failed, skipped, in_progress })
}

async fn counts_raw(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(u32, u32, u32, u32, u32), StoreError> {
    let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*)::bigint, \
         count(*) FILTER (WHERE status = 'succeeded')::bigint, \
         count(*) FILTER (WHERE status IN ('failed', 'stopped'))::bigint, \
         count(*) FILTER (WHERE status = 'skipped')::bigint, \
         count(*) FILTER (WHERE status IN ('pending', 'running'))::bigint \
         FROM replication_tasks WHERE execution_id = $1",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| map_err("count replication tasks", e))?;
    Ok((
        u32::try_from(row.0).unwrap_or(u32::MAX),
        u32::try_from(row.1).unwrap_or(u32::MAX),
        u32::try_from(row.2).unwrap_or(u32::MAX),
        u32::try_from(row.3).unwrap_or(u32::MAX),
        u32::try_from(row.4).unwrap_or(u32::MAX),
    ))
}

fn decode_exec(row: ExecRow) -> Result<ReplicationExecution, StoreError> {
    let mut execution = ReplicationExecution::new(
        ReplicationExecutionId::from_uuid(row.id),
        ReplicationRuleId::from_uuid(row.rule_id),
        decode_trigger(&row.trigger)?,
        row.created_at,
    );
    execution.status = decode_status(&row.status)?;
    execution.stop_requested = row.stop_requested_at.is_some();
    execution.requested_by = row.requested_by.map(UserId::from_uuid);
    execution.started_at = row.started_at;
    execution.finished_at = row.finished_at;
    Ok(execution)
}

fn decode_task(row: TaskRow) -> Result<ReplicationTask, StoreError> {
    let mut task = ReplicationTask::new(
        ReplicationTaskId::from_uuid(row.id),
        ReplicationExecutionId::from_uuid(row.execution_id),
        decode_kind(&row.kind)?,
        row.repository,
        row.image,
        row.reference,
        row.available_at,
    );
    task.status = decode_task_status(&row.status)?;
    task.attempts = u8::try_from(row.attempts).unwrap_or(u8::MAX);
    task.bytes_transferred = u64::try_from(row.bytes_transferred).unwrap_or(0);
    task.error = row.error;
    task.started_at = row.started_at;
    task.finished_at = row.finished_at;
    Ok(task)
}

fn trigger_label(trigger: TaskTrigger) -> Result<&'static str, StoreError> {
    Ok(match trigger {
        TaskTrigger::Schedule => "schedule",
        TaskTrigger::Manual => "manual",
        TaskTrigger::Event => "event",
        TaskTrigger::Startup => "startup",
        _ => return Err(backend("write replication execution", "trigger is unknown")),
    })
}

fn status_label(status: ExecutionStatus) -> Result<&'static str, StoreError> {
    Ok(match status {
        ExecutionStatus::Queued => "queued",
        ExecutionStatus::Running => "running",
        ExecutionStatus::Succeeded => "succeeded",
        ExecutionStatus::PartiallyFailed => "partially_failed",
        ExecutionStatus::Failed => "failed",
        ExecutionStatus::Stopped => "stopped",
        _ => return Err(backend("write replication execution", "status is unknown")),
    })
}

fn task_status_label(status: ReplicationTaskStatus) -> Result<&'static str, StoreError> {
    Ok(match status {
        ReplicationTaskStatus::Pending => "pending",
        ReplicationTaskStatus::Running => "running",
        ReplicationTaskStatus::Succeeded => "succeeded",
        ReplicationTaskStatus::Failed => "failed",
        ReplicationTaskStatus::Skipped => "skipped",
        ReplicationTaskStatus::Stopped => "stopped",
    })
}

fn kind_label(kind: ReplicationTaskKind) -> Result<&'static str, StoreError> {
    Ok(match kind {
        ReplicationTaskKind::Copy => "copy",
        ReplicationTaskKind::CopyReferrers => "copy_referrers",
        ReplicationTaskKind::Delete => "delete",
    })
}

fn decode_trigger(label: &str) -> Result<TaskTrigger, StoreError> {
    Ok(match label {
        "schedule" => TaskTrigger::Schedule,
        "manual" => TaskTrigger::Manual,
        "event" => TaskTrigger::Event,
        "startup" => TaskTrigger::Startup,
        _ => return Err(backend("read replication execution", "trigger is unknown")),
    })
}

fn decode_status(label: &str) -> Result<ExecutionStatus, StoreError> {
    Ok(match label {
        "queued" => ExecutionStatus::Queued,
        "running" => ExecutionStatus::Running,
        "succeeded" => ExecutionStatus::Succeeded,
        "partially_failed" => ExecutionStatus::PartiallyFailed,
        "failed" => ExecutionStatus::Failed,
        "stopped" => ExecutionStatus::Stopped,
        _ => return Err(backend("read replication execution", "status is unknown")),
    })
}

fn decode_kind(label: &str) -> Result<ReplicationTaskKind, StoreError> {
    Ok(match label {
        "copy" => ReplicationTaskKind::Copy,
        "copy_referrers" => ReplicationTaskKind::CopyReferrers,
        "delete" => ReplicationTaskKind::Delete,
        _ => return Err(backend("read replication task", "kind is unknown")),
    })
}

fn decode_task_status(label: &str) -> Result<ReplicationTaskStatus, StoreError> {
    Ok(match label {
        "pending" => ReplicationTaskStatus::Pending,
        "running" => ReplicationTaskStatus::Running,
        "succeeded" => ReplicationTaskStatus::Succeeded,
        "failed" => ReplicationTaskStatus::Failed,
        "skipped" => ReplicationTaskStatus::Skipped,
        "stopped" => ReplicationTaskStatus::Stopped,
        _ => return Err(backend("read replication task", "status is unknown")),
    })
}
