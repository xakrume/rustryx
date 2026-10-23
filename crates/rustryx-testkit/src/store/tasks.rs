//! Task definitions and task runs.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::audit::ActorRef;
use rustryx_core::error::StoreError;
use rustryx_core::events::{EventDraft, EventKind};
use rustryx_core::ids::{TaskDefinitionId, TaskRunId};
use rustryx_core::ports::{Page, PageRequest, TaskDefinitionRepo, TaskRunRepo};
use rustryx_core::tasks::{
    Heartbeat, NewTaskRun, Reaped, TaskDefinition, TaskLogLine, TaskOutcome, TaskRun, TaskRunStatus,
};
use time::OffsetDateTime;

use super::paging::paginate;
use super::state::{State, TaskRunRow};
use super::support::{bump_version, clip_message, read_lines};
use super::{MemStore, conflict, not_found};
use crate::support::add_std;

/// Lines a run log keeps before it is cut.
const MAX_RUN_LOG_LINES: usize = 5000;
/// Longest run error text, in characters.
const MAX_RUN_ERROR_CHARS: usize = 500;
/// A run of a resumable kind is claimed at most this often before it is abandoned.
const MAX_RUN_ATTEMPTS: u8 = 3;
/// Level of the line that marks a cut log (warning, as stored).
const TRUNCATION_LEVEL: u8 = 3;

fn valid_task_kind(kind: &str) -> bool {
    let mut chars = kind.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && kind.chars().count() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[async_trait]
impl TaskDefinitionRepo for MemStore {
    async fn create(&self, definition: &TaskDefinition) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !valid_task_kind(&definition.kind) {
            return Err(conflict("a task kind is lowercase letters, digits and '_'"));
        }
        let taken = state
            .task_definitions
            .values()
            .any(|d| d.id == definition.id || d.name == definition.name);
        if taken {
            return Err(conflict(format!("task {} already exists", definition.name)));
        }
        state
            .task_definitions
            .insert(definition.id, definition.clone());
        Ok(())
    }

    async fn get(&self, id: TaskDefinitionId) -> Result<Option<TaskDefinition>, StoreError> {
        Ok(self.tables()?.task_definitions.get(&id).cloned())
    }

    async fn list(&self, kind: Option<&str>) -> Result<Vec<TaskDefinition>, StoreError> {
        let mut all: Vec<TaskDefinition> = self
            .tables()?
            .task_definitions
            .values()
            .filter(|d| kind.is_none_or(|k| d.kind == k))
            .cloned()
            .collect();
        all.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
        Ok(all)
    }

    async fn update(
        &self,
        definition: &TaskDefinition,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .task_definitions
            .get(&definition.id)
            .ok_or_else(|| not_found("task", definition.id))?;
        let version = bump_version(existing.version, expected_version)?;
        let mut next = existing.clone();
        // What an edit may change: schedule, enabled and notify, and for a task that is not a
        // system task also name and params. The kind, the progress of the schedule
        // (`next_run_at`, `last_run_id`) and the creation time are never taken from the
        // caller, so an edit from a stale read cannot re-fire a schedule.
        next.schedule = definition.schedule.clone();
        next.enabled = definition.enabled;
        next.notify = definition.notify;
        if !existing.system {
            next.name.clone_from(&definition.name);
            next.params.clone_from(&definition.params);
        }
        let name_taken = state
            .task_definitions
            .values()
            .any(|d| d.id != next.id && d.name == next.name);
        if name_taken {
            return Err(conflict(format!("task {} already exists", next.name)));
        }
        next.version = version;
        next.updated_at = now;
        state.task_definitions.insert(next.id, next);
        Ok(())
    }

    async fn delete(&self, id: TaskDefinitionId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let definition = state
            .task_definitions
            .get(&id)
            .ok_or_else(|| not_found("task", id))?;
        if definition.system {
            return Err(conflict("a system task cannot be deleted"));
        }
        state.task_definitions.remove(&id);
        Ok(())
    }

    async fn seed(&self, definitions: &[TaskDefinition]) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let mut inserted = 0_u64;
        for definition in definitions {
            // Existing rows stay as they are: an operator may have edited the schedule.
            let exists = state
                .task_definitions
                .values()
                .any(|d| d.id == definition.id || d.name == definition.name);
            if exists {
                continue;
            }
            if !valid_task_kind(&definition.kind) {
                return Err(conflict("a task kind is lowercase letters, digits and '_'"));
            }
            state
                .task_definitions
                .insert(definition.id, definition.clone());
            inserted += 1;
        }
        Ok(inserted)
    }

    async fn due(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<TaskDefinition>, StoreError> {
        let state = self.tables()?;
        let mut due: Vec<TaskDefinition> = state
            .task_definitions
            .values()
            .filter(|d| d.enabled && d.next_run_at.is_some_and(|at| at <= now))
            .cloned()
            .collect();
        due.sort_by_key(|d| (d.next_run_at, d.id));
        due.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(due)
    }

    async fn advance(
        &self,
        id: TaskDefinitionId,
        expected: Option<OffsetDateTime>,
        next: Option<OffsetDateTime>,
    ) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        // Compare and set: exactly one node wins a due time. A definition deleted meanwhile
        // is a lost race, not an error.
        let Some(definition) = state.task_definitions.get_mut(&id) else {
            return Ok(false);
        };
        if definition.next_run_at != expected {
            return Ok(false);
        }
        definition.next_run_at = next;
        Ok(true)
    }
}

impl State {
    fn run_index(&self, id: TaskRunId) -> Option<usize> {
        self.task_runs.iter().position(|row| row.run.id == id)
    }

    /// The running row `id` that `node_id` still holds.
    fn held_run(&mut self, id: TaskRunId, node_id: &str) -> Option<&mut TaskRunRow> {
        self.task_runs.iter_mut().find(|row| {
            row.run.id == id
                && row.run.status == TaskRunStatus::Running
                && row.run.node_id.as_deref() == Some(node_id)
        })
    }
}

fn run_key(seq: u64) -> String {
    format!("{:020}", u64::MAX - seq)
}

#[async_trait]
impl TaskRunRepo for MemStore {
    async fn enqueue(&self, run: &NewTaskRun) -> Result<Option<TaskRun>, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        // The partial unique index on `(kind, scope_key)` for queued and running rows.
        let active = state.task_runs.iter().any(|row| {
            row.run.kind == run.kind
                && row.run.scope_key == run.scope_key
                && !row.run.status.is_terminal()
        });
        if active {
            return Ok(None);
        }
        let mut created = TaskRun::new(TaskRunId::generate(), run.kind.clone(), run.trigger, now);
        created.task_id = run.task_id;
        created.scope_key.clone_from(&run.scope_key);
        created.dry_run = run.dry_run;
        created.params.clone_from(&run.params);
        state.run_seq += 1;
        let seq = state.run_seq;
        state.task_runs.push(TaskRunRow {
            seq,
            run: created.clone(),
            available_at: now,
            lease_expires_at: None,
            attempts: 0,
        });
        if let Some(definition) = run
            .task_id
            .and_then(|id| state.task_definitions.get_mut(&id))
        {
            definition.last_run_id = Some(created.id);
        }
        Ok(Some(created))
    }

    async fn claim(
        &self,
        node_id: &str,
        kinds: &[&str],
        lease: Duration,
    ) -> Result<Option<TaskRun>, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let candidate = state
            .task_runs
            .iter_mut()
            .filter(|row| row.run.status == TaskRunStatus::Queued && row.available_at <= now)
            .filter(|row| kinds.contains(&row.run.kind.as_str()))
            .min_by_key(|row| (row.run.queued_at, row.seq));
        let Some(row) = candidate else {
            return Ok(None);
        };
        row.run.status = TaskRunStatus::Running;
        row.run.node_id = Some(node_id.to_owned());
        row.run.started_at = Some(now);
        row.lease_expires_at = Some(add_std(now, lease));
        row.attempts = row.attempts.saturating_add(1);
        Ok(Some(row.run.clone()))
    }

    async fn release(
        &self,
        id: TaskRunId,
        node_id: &str,
        delay: Duration,
    ) -> Result<bool, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let Some(row) = state.held_run(id, node_id) else {
            return Ok(false);
        };
        row.run.status = TaskRunStatus::Queued;
        row.run.node_id = None;
        row.run.started_at = None;
        row.lease_expires_at = None;
        // Handing a run back because the singleton lock was busy is not an attempt.
        row.attempts = row.attempts.saturating_sub(1);
        row.available_at = add_std(now, delay);
        Ok(true)
    }

    async fn heartbeat(
        &self,
        id: TaskRunId,
        node_id: &str,
        lease: Duration,
    ) -> Result<Heartbeat, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let Some(row) = state.held_run(id, node_id) else {
            return Ok(Heartbeat {
                owned: false,
                stop_requested: false,
            });
        };
        row.lease_expires_at = Some(add_std(now, lease));
        Ok(Heartbeat {
            owned: true,
            stop_requested: row.run.stop_requested,
        })
    }

    async fn finish(
        &self,
        id: TaskRunId,
        node_id: &str,
        outcome: &TaskOutcome,
    ) -> Result<bool, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if !outcome.status.is_terminal() {
            return Err(StoreError::Invalid(
                "a run finishes with a terminal status".to_owned(),
            ));
        }
        let Some(row) = state.held_run(id, node_id) else {
            return Ok(false);
        };
        row.run.status = outcome.status;
        row.run.summary.clone_from(&outcome.summary);
        row.run.error = outcome
            .error
            .as_ref()
            .map(|e| e.chars().take(MAX_RUN_ERROR_CHARS).collect());
        row.run.finished_at = Some(now);
        row.lease_expires_at = None;
        let run = row.run.clone();
        // The `task.finished` event goes out in the same step when the definition asks for it.
        let wants_event = run
            .task_id
            .and_then(|task| state.task_definitions.get(&task))
            .is_some_and(|definition| match outcome.status {
                TaskRunStatus::Failed => definition.notify.on_failure,
                TaskRunStatus::Succeeded => definition.notify.on_success,
                _ => false,
            });
        if wants_event {
            let draft = EventDraft::new(
                EventKind::TaskFinished,
                ActorRef::system(format!("task:{}", run.kind)),
                serde_json::json!({
                    "run_id": run.id,
                    "kind": run.kind,
                    "status": outcome.status,
                }),
            );
            state.push_events(&[draft], now);
        }
        Ok(true)
    }

    async fn request_stop(&self, id: TaskRunId) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        let index = state
            .run_index(id)
            .ok_or_else(|| not_found("task run", id))?;
        let row = &mut state.task_runs[index];
        if row.run.status.is_terminal() {
            return Ok(false);
        }
        row.run.stop_requested = true;
        Ok(true)
    }

    async fn get(&self, id: TaskRunId) -> Result<Option<TaskRun>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .run_index(id)
            .map(|index| state.task_runs[index].run.clone()))
    }

    async fn list(
        &self,
        task: Option<TaskDefinitionId>,
        page: &PageRequest,
    ) -> Result<Page<TaskRun>, StoreError> {
        let rows: Vec<(String, TaskRun)> = self
            .tables()?
            .task_runs
            .iter()
            .filter(|row| task.is_none_or(|t| row.run.task_id == Some(t)))
            .map(|row| (run_key(row.seq), row.run.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn append_log(&self, id: TaskRunId, lines: &[TaskLogLine]) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        if state.run_index(id).is_none() {
            return Err(not_found("task run", id));
        }
        let log = state.task_logs.entry(id).or_default();
        for line in lines {
            if log.len() > MAX_RUN_LOG_LINES {
                // The truncation line is already there: later lines are dropped.
                break;
            }
            if log.len() == MAX_RUN_LOG_LINES {
                let next_seq = log.last().map_or(0, |last| last.seq.saturating_add(1));
                log.push(TaskLogLine {
                    seq: next_seq,
                    at: now,
                    level: TRUNCATION_LEVEL,
                    message: "log truncated".to_owned(),
                });
                break;
            }
            let mut stored = line.clone();
            stored.message = clip_message(&line.message);
            log.push(stored);
        }
        Ok(())
    }

    async fn read_log(
        &self,
        id: TaskRunId,
        after_seq: i32,
        limit: u32,
    ) -> Result<Vec<TaskLogLine>, StoreError> {
        let state = self.tables()?;
        if state.run_index(id).is_none() {
            return Err(not_found("task run", id));
        }
        Ok(read_lines(
            state.task_logs.get(&id).map_or(&[][..], Vec::as_slice),
            after_seq,
            limit,
        ))
    }

    async fn reap_expired(
        &self,
        now: OffsetDateTime,
        resumable_kinds: &[&str],
    ) -> Result<Reaped, StoreError> {
        let mut state = self.tables()?;
        let mut reaped = Reaped {
            requeued: 0,
            abandoned: 0,
        };
        for row in &mut state.task_runs {
            let expired = row.run.status == TaskRunStatus::Running
                && row.lease_expires_at.is_some_and(|at| at <= now);
            if !expired {
                continue;
            }
            let resumable =
                resumable_kinds.contains(&row.run.kind.as_str()) && row.attempts < MAX_RUN_ATTEMPTS;
            row.lease_expires_at = None;
            row.run.node_id = None;
            if resumable {
                row.run.status = TaskRunStatus::Queued;
                row.run.started_at = None;
                row.available_at = now;
                reaped.requeued += 1;
            } else {
                row.run.status = TaskRunStatus::Abandoned;
                row.run.finished_at = Some(now);
                row.run.error = Some("lease_expired".to_owned());
                reaped.abandoned += 1;
            }
        }
        Ok(reaped)
    }

    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let doomed: Vec<TaskRunId> = state
            .task_runs
            .iter()
            .filter(|row| row.run.finished_at.is_some_and(|at| at < finished_before))
            .map(|row| row.run.id)
            .collect();
        state.task_runs.retain(|row| !doomed.contains(&row.run.id));
        for id in &doomed {
            state.task_logs.remove(id);
        }
        Ok(u64::try_from(doomed.len()).unwrap_or(u64::MAX))
    }
}
