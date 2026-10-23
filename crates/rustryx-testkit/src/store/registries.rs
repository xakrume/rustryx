//! Registries and replication.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::audit::ActorRef;
use rustryx_core::error::StoreError;
use rustryx_core::events::{CursorAdvance, EventDraft, EventKind};
use rustryx_core::ids::{RegistryId, ReplicationExecutionId, ReplicationRuleId, ReplicationTaskId};
use rustryx_core::model::UpstreamAuth;
use rustryx_core::ports::{Page, PageRequest, RegistryDependents, RegistryRepo, ReplicationRepo};
use rustryx_core::registry::{Registry, RegistryAuth, RegistryTest};
use rustryx_core::replication::{
    ExecutionCounts, ExecutionStatus, MAX_TASK_ERROR_CHARS, ReplicationExecution, ReplicationRule,
    ReplicationScope, ReplicationTask, ReplicationTaskStatus,
};
use rustryx_core::tasks::{TaskLogLine, TaskTrigger};
use time::OffsetDateTime;

use super::outbox::validate_drafts;
use super::paging::paginate;
use super::state::{State, TaskRow};
use super::support::{bump_version, clip_message, read_lines};
use super::{MemStore, conflict, not_found};
use crate::support::add_std;

/// Most tasks one `add_tasks` call takes.
const MAX_TASKS_PER_CALL: usize = 1000;
/// An event execution that is empty this long is settled, so a rule never stays blocked by an
/// execution that never received a task.
const EMPTY_EXECUTION_GRACE: Duration = Duration::from_secs(60);

fn upstream_auth(auth: &RegistryAuth) -> Option<UpstreamAuth> {
    match auth {
        RegistryAuth::Basic { username, password } => Some(UpstreamAuth {
            username: username.clone(),
            password: password.clone(),
        }),
        // A proxy repository carries basic credentials only; a bearer registry is reached
        // with the registry record itself.
        RegistryAuth::None | RegistryAuth::Bearer { .. } => None,
    }
}

#[async_trait]
impl RegistryRepo for MemStore {
    async fn create(&self, registry: &Registry) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        registry
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let taken = state
            .registries
            .values()
            .any(|r| r.id == registry.id || r.name == registry.name);
        if taken {
            return Err(conflict(format!(
                "registry {} already exists",
                registry.name
            )));
        }
        state.registries.insert(registry.id, registry.clone());
        Ok(())
    }

    async fn get(&self, id: RegistryId) -> Result<Option<Registry>, StoreError> {
        Ok(self.tables()?.registries.get(&id).cloned())
    }

    async fn list(&self) -> Result<Vec<Registry>, StoreError> {
        let mut all: Vec<Registry> = self.tables()?.registries.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn update(&self, registry: &Registry, expected_version: i64) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .registries
            .get(&registry.id)
            .ok_or_else(|| not_found("registry", registry.id))?;
        let version = bump_version(existing.version, expected_version)?;
        registry
            .validate()
            .map_err(|error| conflict(error.to_string()))?;
        let name_taken = state
            .registries
            .values()
            .any(|r| r.id != registry.id && r.name == registry.name);
        if name_taken {
            return Err(conflict(format!(
                "registry {} already exists",
                registry.name
            )));
        }
        let mut next = registry.clone();
        next.version = version;
        next.created_at = existing.created_at;
        next.last_test.clone_from(&existing.last_test);
        // The proxy repositories keep an inline copy of the upstream, so the same step
        // rewrites every bound one: a repository must never fetch from the old address.
        for repository in state.repositories.values_mut() {
            if let Some(proxy) = repository
                .proxy
                .as_mut()
                .filter(|proxy| proxy.registry_id == Some(next.id))
            {
                proxy.remote_url.clone_from(&next.url);
                proxy.auth = upstream_auth(&next.auth);
                proxy
                    .ca_certificate_ids
                    .clone_from(&next.ca_certificate_ids);
                repository.updated_at = now;
            }
        }
        state.registries.insert(next.id, next);
        drop(state);
        self.notify(rustryx_core::ports::InvalidationEvent::Reset);
        Ok(())
    }

    async fn delete(&self, id: RegistryId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.registries.contains_key(&id) {
            return Err(not_found("registry", id));
        }
        let dependents = state.registry_dependents(id);
        if !dependents.proxy_repositories.is_empty() || !dependents.replication_rules.is_empty() {
            return Err(conflict("registry is still in use"));
        }
        state.registries.remove(&id);
        Ok(())
    }

    async fn dependents(&self, id: RegistryId) -> Result<RegistryDependents, StoreError> {
        Ok(self.tables()?.registry_dependents(id))
    }

    async fn record_test(&self, id: RegistryId, test: &RegistryTest) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let registry = state
            .registries
            .get_mut(&id)
            .ok_or_else(|| not_found("registry", id))?;
        registry.last_test = Some(test.clone());
        Ok(())
    }
}

impl State {
    pub(crate) fn registry_dependents(&self, id: RegistryId) -> RegistryDependents {
        let mut proxy_repositories: Vec<_> = self
            .repositories
            .values()
            .filter(|r| r.proxy.as_ref().is_some_and(|p| p.registry_id == Some(id)))
            .map(|r| r.id)
            .collect();
        proxy_repositories.sort();
        let mut replication_rules: Vec<_> = self
            .replication_rules
            .values()
            .filter(|rule| rule.registry_id == id)
            .map(|rule| rule.id)
            .collect();
        replication_rules.sort();
        RegistryDependents {
            proxy_repositories,
            replication_rules,
        }
    }

    fn execution_index(&self, id: ReplicationExecutionId) -> Option<usize> {
        self.executions.iter().position(|(_, e)| e.id == id)
    }

    fn active_execution_of(&self, rule: ReplicationRuleId) -> Option<usize> {
        self.executions
            .iter()
            .position(|(_, e)| e.rule_id == rule && !e.status.is_terminal())
    }

    /// The counters of an execution, computed from its tasks so that they cannot drift.
    fn execution_counts(&self, id: ReplicationExecutionId) -> ExecutionCounts {
        let mut counts = ExecutionCounts::default();
        for row in self
            .replication_tasks
            .iter()
            .filter(|r| r.task.execution_id == id)
        {
            counts.total = counts.total.saturating_add(1);
            match row.task.status {
                ReplicationTaskStatus::Succeeded => {
                    counts.succeeded = counts.succeeded.saturating_add(1);
                }
                ReplicationTaskStatus::Failed | ReplicationTaskStatus::Stopped => {
                    counts.failed = counts.failed.saturating_add(1);
                }
                ReplicationTaskStatus::Skipped => counts.skipped = counts.skipped.saturating_add(1),
                ReplicationTaskStatus::Pending | ReplicationTaskStatus::Running => {
                    counts.in_progress = counts.in_progress.saturating_add(1);
                }
            }
        }
        counts
    }

    fn execution_view(&self, execution: &ReplicationExecution) -> ReplicationExecution {
        let mut view = execution.clone();
        view.counts = self.execution_counts(execution.id);
        view
    }

    fn scope_exists(&self, scope: ReplicationScope) -> bool {
        match scope {
            ReplicationScope::Repository(id) => self.repositories.contains_key(&id),
            ReplicationScope::Project(id) => self.projects.contains_key(&id),
        }
    }

    fn held_task(&mut self, id: ReplicationTaskId, owner: &str) -> Option<&mut TaskRow> {
        self.replication_tasks.iter_mut().find(|row| {
            row.task.id == id
                && row.task.status == ReplicationTaskStatus::Running
                && row.owner.as_deref() == Some(owner)
        })
    }

    fn insert_tasks(&mut self, tasks: &[ReplicationTask]) -> Result<(), StoreError> {
        for task in tasks {
            task.validate()
                .map_err(|error| conflict(error.to_string()))?;
            if self.execution_index(task.execution_id).is_none() {
                return Err(conflict(format!(
                    "execution {} does not exist",
                    task.execution_id
                )));
            }
            if self.replication_tasks.iter().any(|r| r.task.id == task.id) {
                return Err(conflict(format!("task {} already exists", task.id)));
            }
        }
        for task in tasks {
            self.replication_tasks.push(TaskRow {
                task: task.clone(),
                owner: None,
                lease_expires_at: None,
            });
        }
        Ok(())
    }
}

fn execution_key(seq: u64) -> String {
    format!("{:020}", u64::MAX - seq)
}

#[async_trait]
impl ReplicationRepo for MemStore {
    async fn create_rule(&self, rule: &ReplicationRule) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        rule.validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.registries.contains_key(&rule.registry_id) || !state.scope_exists(rule.local) {
            return Err(conflict("the registry and the local scope must exist"));
        }
        let taken = state
            .replication_rules
            .values()
            .any(|r| r.id == rule.id || r.name == rule.name);
        if taken {
            return Err(conflict(format!("rule {} already exists", rule.name)));
        }
        state.replication_rules.insert(rule.id, rule.clone());
        Ok(())
    }

    async fn get_rule(&self, id: ReplicationRuleId) -> Result<Option<ReplicationRule>, StoreError> {
        Ok(self.tables()?.replication_rules.get(&id).cloned())
    }

    async fn list_rules(&self) -> Result<Vec<ReplicationRule>, StoreError> {
        let mut all: Vec<ReplicationRule> =
            self.tables()?.replication_rules.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all)
    }

    async fn update_rule(
        &self,
        rule: &ReplicationRule,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let existing = state
            .replication_rules
            .get(&rule.id)
            .ok_or_else(|| not_found("replication rule", rule.id))?;
        let version = bump_version(existing.version, expected_version)?;
        // The rule cannot change under a run.
        if state.active_execution_of(rule.id).is_some() {
            return Err(conflict(
                "the rule has an execution that is queued or running",
            ));
        }
        rule.validate()
            .map_err(|error| conflict(error.to_string()))?;
        if !state.registries.contains_key(&rule.registry_id) || !state.scope_exists(rule.local) {
            return Err(conflict("the registry and the local scope must exist"));
        }
        let name_taken = state
            .replication_rules
            .values()
            .any(|r| r.id != rule.id && r.name == rule.name);
        if name_taken {
            return Err(conflict(format!("rule {} already exists", rule.name)));
        }
        let mut next = rule.clone();
        next.version = version;
        next.created_at = existing.created_at;
        next.created_by = existing.created_by;
        next.updated_at = now;
        state.replication_rules.insert(next.id, next);
        Ok(())
    }

    async fn delete_rule(&self, id: ReplicationRuleId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.replication_rules.remove(&id).is_none() {
            return Err(not_found("replication rule", id));
        }
        // Executions, their tasks and the task logs go with the rule (`ON DELETE CASCADE`).
        let doomed: HashSet<ReplicationExecutionId> = state
            .executions
            .iter()
            .filter(|(_, e)| e.rule_id == id)
            .map(|(_, e)| e.id)
            .collect();
        state.executions.retain(|(_, e)| !doomed.contains(&e.id));
        let gone: HashSet<ReplicationTaskId> = state
            .replication_tasks
            .iter()
            .filter(|r| doomed.contains(&r.task.execution_id))
            .map(|r| r.task.id)
            .collect();
        state
            .replication_tasks
            .retain(|r| !doomed.contains(&r.task.execution_id));
        state
            .replication_logs
            .retain(|task, _| !gone.contains(task));
        Ok(())
    }

    async fn start_execution(
        &self,
        execution: &ReplicationExecution,
    ) -> Result<Option<ReplicationExecution>, StoreError> {
        let mut state = self.tables()?;
        if !state.replication_rules.contains_key(&execution.rule_id) {
            return Err(conflict(format!(
                "rule {} does not exist",
                execution.rule_id
            )));
        }
        // The partial unique index: one queued or running execution per rule.
        if state.active_execution_of(execution.rule_id).is_some() {
            return Ok(None);
        }
        if state.execution_index(execution.id).is_some() {
            return Err(conflict(format!(
                "execution {} already exists",
                execution.id
            )));
        }
        state.run_seq += 1;
        let seq = state.run_seq;
        let mut stored = execution.clone();
        stored.counts = ExecutionCounts::default();
        state.executions.push((seq, stored.clone()));
        Ok(Some(stored))
    }

    async fn get_execution(
        &self,
        id: ReplicationExecutionId,
    ) -> Result<Option<ReplicationExecution>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .execution_index(id)
            .map(|index| state.execution_view(&state.executions[index].1)))
    }

    async fn list_executions(
        &self,
        rule: Option<ReplicationRuleId>,
        page: &PageRequest,
    ) -> Result<Page<ReplicationExecution>, StoreError> {
        let state = self.tables()?;
        let rows: Vec<(String, ReplicationExecution)> = state
            .executions
            .iter()
            .filter(|(_, e)| rule.is_none_or(|r| e.rule_id == r))
            .map(|(seq, e)| (execution_key(*seq), state.execution_view(e)))
            .collect();
        drop(state);
        paginate(rows, page)
    }

    async fn request_stop(&self, id: ReplicationExecutionId) -> Result<bool, StoreError> {
        let mut state = self.tables()?;
        let index = state
            .execution_index(id)
            .ok_or_else(|| not_found("replication execution", id))?;
        let execution = &mut state.executions[index].1;
        if execution.status.is_terminal() {
            return Ok(false);
        }
        execution.stop_requested = true;
        Ok(true)
    }

    async fn add_tasks(&self, tasks: &[ReplicationTask]) -> Result<(), StoreError> {
        if tasks.len() > MAX_TASKS_PER_CALL {
            return Err(StoreError::Invalid(
                "at most 1000 tasks per call".to_owned(),
            ));
        }
        self.tables()?.insert_tasks(tasks)
    }

    async fn create_event_tasks(
        &self,
        rule: ReplicationRuleId,
        tasks: &[ReplicationTask],
        advance: Option<&CursorAdvance>,
    ) -> Result<u64, StoreError> {
        if tasks.len() > MAX_TASKS_PER_CALL {
            return Err(StoreError::Invalid(
                "at most 1000 tasks per call".to_owned(),
            ));
        }
        let now = self.now();
        let mut state = self.tables()?;
        if !state.replication_rules.contains_key(&rule) {
            return Err(not_found("replication rule", rule));
        }
        // The cursor decides the whole call: a duplicate consumer that lost the compare and
        // set writes nothing.
        if let Some(advance) = advance {
            let current = state
                .outbox_cursors
                .get(advance.consumer)
                .copied()
                .unwrap_or_default();
            if current != advance.from {
                return Ok(0);
            }
        }
        // Events join the rule's open execution. The unique index allows one active execution
        // per rule, so a new event execution exists only when none is active; the one minute
        // window of the real job only groups bursts and cannot be told apart here.
        let execution = if let Some(index) = state.active_execution_of(rule) {
            state.executions[index].1.id
        } else {
            state.run_seq += 1;
            let seq = state.run_seq;
            let created = ReplicationExecution::new(
                ReplicationExecutionId::generate(),
                rule,
                TaskTrigger::Event,
                now,
            );
            let id = created.id;
            state.executions.push((seq, created));
            id
        };
        let assigned: Vec<ReplicationTask> = tasks
            .iter()
            .map(|task| {
                let mut row = task.clone();
                row.execution_id = execution;
                row
            })
            .collect();
        state.insert_tasks(&assigned)?;
        if let Some(advance) = advance {
            state
                .outbox_cursors
                .insert(advance.consumer.to_owned(), advance.to);
        }
        Ok(u64::try_from(assigned.len()).unwrap_or(u64::MAX))
    }

    async fn claim_task(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<Option<ReplicationTask>, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let runnable: HashSet<ReplicationExecutionId> = state
            .executions
            .iter()
            .filter(|(_, e)| !e.status.is_terminal() && !e.stop_requested)
            .map(|(_, e)| e.id)
            .collect();
        let candidate = state
            .replication_tasks
            .iter()
            .enumerate()
            .filter(|(_, row)| runnable.contains(&row.task.execution_id))
            .filter(|(_, row)| match row.task.status {
                ReplicationTaskStatus::Pending => row.task.available_at <= now,
                // A worker that died loses nothing: its task comes back after the lease.
                ReplicationTaskStatus::Running => row.lease_expires_at.is_some_and(|at| at <= now),
                _ => false,
            })
            .min_by_key(|(_, row)| (row.task.available_at, row.task.id))
            .map(|(index, _)| index);
        let Some(index) = candidate else {
            return Ok(None);
        };
        let execution = state.replication_tasks[index].task.execution_id;
        let row = &mut state.replication_tasks[index];
        row.task.status = ReplicationTaskStatus::Running;
        row.task.attempts = row.task.attempts.saturating_add(1);
        row.task.started_at.get_or_insert(now);
        row.owner = Some(owner.to_owned());
        row.lease_expires_at = Some(add_std(now, lease));
        let claimed = row.task.clone();
        if let Some(index) = state.execution_index(execution) {
            let run = &mut state.executions[index].1;
            if run.status == ExecutionStatus::Queued {
                run.status = ExecutionStatus::Running;
                run.started_at = Some(now);
            }
        }
        Ok(Some(claimed))
    }

    async fn heartbeat_task(
        &self,
        id: ReplicationTaskId,
        owner: &str,
        lease: Duration,
    ) -> Result<bool, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let Some(row) = state.held_task(id, owner) else {
            return Ok(false);
        };
        row.lease_expires_at = Some(add_std(now, lease));
        Ok(true)
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
        validate_drafts(events)?;
        let now = self.now();
        let mut state = self.tables()?;
        // `retry_in` puts the task back as pending, anything else ends it, so a status that is
        // not terminal without a retry would leave the task claimed forever.
        if retry_in.is_none() && !status.is_terminal() {
            return Err(StoreError::Invalid(
                "a task ends with a terminal status or goes back with a retry delay".to_owned(),
            ));
        }
        let Some(row) = state.held_task(id, owner) else {
            return Ok(false);
        };
        row.task.bytes_transferred = bytes;
        // An error is a code of at most 64 characters, cut like the column would refuse it.
        row.task.error = error.map(|e| e.chars().take(MAX_TASK_ERROR_CHARS).collect());
        row.owner = None;
        row.lease_expires_at = None;
        if let Some(delay) = retry_in {
            row.task.status = ReplicationTaskStatus::Pending;
            row.task.available_at = add_std(now, delay);
        } else {
            row.task.status = status;
            row.task.finished_at = Some(now);
        }
        state.push_events(events, now);
        Ok(true)
    }

    async fn tasks(
        &self,
        execution: ReplicationExecutionId,
        page: &PageRequest,
    ) -> Result<Page<ReplicationTask>, StoreError> {
        let rows: Vec<(String, ReplicationTask)> = self
            .tables()?
            .replication_tasks
            .iter()
            .filter(|row| row.task.execution_id == execution)
            .map(|row| (row.task.id.to_string(), row.task.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn append_task_log(
        &self,
        id: ReplicationTaskId,
        lines: &[TaskLogLine],
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if !state.replication_tasks.iter().any(|r| r.task.id == id) {
            return Err(not_found("replication task", id));
        }
        let log = state.replication_logs.entry(id).or_default();
        for line in lines {
            let mut stored = line.clone();
            stored.message = clip_message(&line.message);
            log.push(stored);
        }
        Ok(())
    }

    async fn read_task_log(
        &self,
        id: ReplicationTaskId,
        after_seq: i32,
        limit: u32,
    ) -> Result<Vec<TaskLogLine>, StoreError> {
        let state = self.tables()?;
        if !state.replication_tasks.iter().any(|r| r.task.id == id) {
            return Err(not_found("replication task", id));
        }
        Ok(read_lines(
            state
                .replication_logs
                .get(&id)
                .map_or(&[][..], Vec::as_slice),
            after_seq,
            limit,
        ))
    }

    async fn settle_executions(&self, now: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let open: Vec<ReplicationExecutionId> = state
            .executions
            .iter()
            .filter(|(_, e)| !e.status.is_terminal())
            .map(|(_, e)| e.id)
            .collect();
        let mut settled = 0_u64;
        for id in open {
            let Some(index) = state.execution_index(id) else {
                continue;
            };
            let stop = state.executions[index].1.stop_requested;
            let created_at = state.executions[index].1.created_at;
            if stop {
                // Nobody claims the tasks of a stopped execution, so they end here.
                for row in &mut state.replication_tasks {
                    if row.task.execution_id == id
                        && row.task.status == ReplicationTaskStatus::Pending
                    {
                        row.task.status = ReplicationTaskStatus::Stopped;
                        row.task.finished_at = Some(now);
                    }
                }
            }
            let counts = state.execution_counts(id);
            let has_tasks = counts.total > 0;
            // An execution that never received a task is settled only after a grace period,
            // because `start_execution` and `add_tasks` are two calls.
            let empty_and_young = !has_tasks
                && Duration::try_from(now - created_at)
                    .is_ok_and(|age| age < EMPTY_EXECUTION_GRACE);
            if counts.in_progress > 0 || empty_and_young {
                continue;
            }
            let status = if stop {
                ExecutionStatus::Stopped
            } else if counts.failed == 0 {
                ExecutionStatus::Succeeded
            } else if counts.succeeded + counts.skipped == 0 {
                ExecutionStatus::Failed
            } else {
                ExecutionStatus::PartiallyFailed
            };
            let rule = state.executions[index].1.rule_id;
            let run = &mut state.executions[index].1;
            run.status = status;
            run.finished_at = Some(now);
            let draft = EventDraft::new(
                EventKind::ReplicationFinished,
                ActorRef::system("replication"),
                serde_json::json!({
                    "execution_id": id,
                    "rule_id": rule,
                    "status": status,
                    "total": counts.total,
                    "succeeded": counts.succeeded,
                    "failed": counts.failed,
                    "skipped": counts.skipped,
                }),
            );
            state.push_events(&[draft], now);
            settled += 1;
        }
        Ok(settled)
    }

    async fn purge(&self, finished_before: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let doomed: HashSet<ReplicationExecutionId> = state
            .executions
            .iter()
            .filter(|(_, e)| e.finished_at.is_some_and(|at| at < finished_before))
            .map(|(_, e)| e.id)
            .collect();
        state.executions.retain(|(_, e)| !doomed.contains(&e.id));
        let gone: HashSet<ReplicationTaskId> = state
            .replication_tasks
            .iter()
            .filter(|r| doomed.contains(&r.task.execution_id))
            .map(|r| r.task.id)
            .collect();
        state
            .replication_tasks
            .retain(|r| !doomed.contains(&r.task.execution_id));
        state
            .replication_logs
            .retain(|task, _| !gone.contains(task));
        Ok(u64::try_from(doomed.len()).unwrap_or(u64::MAX))
    }
}
