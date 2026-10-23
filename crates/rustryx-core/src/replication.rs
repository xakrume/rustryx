//! Replication rules, executions and tasks.
//!
//! A rule copies images between a local scope and a registry. One active execution per rule is
//! enforced by a unique index; every artifact is a task on a lease queue, so a worker that dies
//! loses nothing.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::glob;
use crate::ids::{
    LabelId, ProjectId, RegistryId, ReplicationExecutionId, ReplicationRuleId, ReplicationTaskId,
    RepositoryId, TaskDefinitionId, UserId,
};
use crate::lifecycle::MAX_SELECTOR_GLOBS;
use crate::tasks::TaskTrigger;

/// Longest rule name, in characters.
pub const MAX_RULE_NAME_CHARS: usize = 128;
/// Longest remote namespace, in characters.
pub const MAX_REMOTE_NAMESPACE_CHARS: usize = 255;
/// Most leading path segments a rule may flatten.
pub const MAX_FLATTEN_LEVELS: u8 = 3;
/// Longest wait after a push before an event rule copies it, in seconds.
pub const MAX_EVENT_SETTLE_SECS: u32 = 600;
/// Default wait after a push, so a signature pushed right after can travel with it.
pub const DEFAULT_EVENT_SETTLE_SECS: u32 = 30;
/// Longest stored task error code, in characters.
pub const MAX_TASK_ERROR_CHARS: usize = 64;

/// Which way a rule copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationDirection {
    /// From the local scope to the registry.
    Push,
    /// From the registry to the local scope.
    Pull,
}

/// Which kind of artifact a filter keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceFilter {
    /// Container images.
    Image,
    /// Helm charts.
    Chart,
    /// Both.
    Any,
}

/// Filters of a rule. Globs use the selector syntax of [`crate::glob`]. A label filter keeps
/// only components that carry one of the labels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplicationFilter {
    /// Globs on the image path.
    #[serde(default)]
    pub name_globs: Vec<String>,
    /// Globs on the tag.
    #[serde(default)]
    pub tag_globs: Vec<String>,
    /// Keep only components with one of these labels.
    #[serde(default)]
    pub label_ids: Vec<LabelId>,
    /// Image, chart or both.
    #[serde(default)]
    pub resource: Option<ResourceFilter>,
}

impl ReplicationFilter {
    /// Checks every glob (at most 16 per list, each valid).
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        for globs in [&self.name_globs, &self.tag_globs] {
            if globs.len() > MAX_SELECTOR_GLOBS {
                return Err(DomainError::invalid(
                    "a replication filter has at most 16 globs per list",
                ));
            }
            globs.iter().try_for_each(|g| glob::validate(g))?;
        }
        Ok(())
    }
}

/// What starts a rule. `Scheduled` rules keep their cron in a `task_definitions` row of kind
/// `replication` (`schedule_task_id`), so schedules of every kind live in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ReplicationTrigger {
    /// Only when someone starts it.
    Manual,
    /// By its schedule.
    Scheduled,
    /// By push or delete events.
    Event {
        /// React to pushes.
        on_push: bool,
        /// React to deletes.
        on_delete: bool,
    },
}

/// The local side of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ReplicationScope {
    /// One repository.
    Repository(RepositoryId),
    /// Every repository of a project.
    Project(ProjectId),
}

/// A replication rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the four flags are independent columns of the stored rule (override, referrers, deletion, enabled)"
)]
pub struct ReplicationRule {
    /// Identifier.
    pub id: ReplicationRuleId,
    /// Unique name.
    pub name: String,
    /// Push or pull.
    pub direction: ReplicationDirection,
    /// The remote side.
    pub registry_id: RegistryId,
    /// The local side.
    pub local: ReplicationScope,
    /// Prefix on the remote side (push: where it goes; pull: where it comes from).
    pub remote_namespace: String,
    /// Leading path segments of the image path dropped on the destination, 0 to 3.
    pub flatten_levels: u8,
    /// Replace an existing tag with another digest (subject to the destination's own
    /// immutability).
    pub override_existing: bool,
    /// What to copy.
    pub filter: ReplicationFilter,
    /// What starts it.
    pub trigger: ReplicationTrigger,
    /// The task definition that holds the cron of a scheduled rule.
    pub schedule_task_id: Option<TaskDefinitionId>,
    /// Copy signatures and SBOMs (referrers) after the subject. Default true.
    pub copy_referrers: bool,
    /// Event rules only: propagate deletes. Needs `nx-admin` and is audited.
    pub replicate_deletion: bool,
    /// Transfer speed limit in kilobytes per second, `None` for unlimited.
    pub bandwidth_limit_kbps: Option<u32>,
    /// Event rules: wait this long after a push so a signature pushed right after can travel
    /// with it. Default 30.
    pub event_settle_secs: u32,
    /// A disabled rule starts nothing.
    pub enabled: bool,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creator.
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl ReplicationRule {
    /// A manual, enabled rule that copies referrers, never deletes remotely, never overrides
    /// an existing tag, flattens nothing and waits 30 seconds after an event.
    #[must_use]
    pub fn new(
        id: ReplicationRuleId,
        name: impl Into<String>,
        direction: ReplicationDirection,
        registry_id: RegistryId,
        local: ReplicationScope,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            direction,
            registry_id,
            local,
            remote_namespace: String::new(),
            flatten_levels: 0,
            override_existing: false,
            filter: ReplicationFilter::default(),
            trigger: ReplicationTrigger::Manual,
            schedule_task_id: None,
            copy_referrers: true,
            replicate_deletion: false,
            bandwidth_limit_kbps: None,
            event_settle_secs: DEFAULT_EVENT_SETTLE_SECS,
            enabled: true,
            version: 1,
            created_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Checks the limits of the table constraints, the filter globs, that an event rule reacts
    /// to something, and that only an event rule that reacts to deletes may replicate them.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] naming the broken rule.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.name.is_empty() || self.name.chars().count() > MAX_RULE_NAME_CHARS {
            return Err(DomainError::invalid(
                "rule name must be 1 to 128 characters",
            ));
        }
        if self.remote_namespace.chars().count() > MAX_REMOTE_NAMESPACE_CHARS {
            return Err(DomainError::invalid(
                "remote namespace must be at most 255 characters",
            ));
        }
        if self.flatten_levels > MAX_FLATTEN_LEVELS {
            return Err(DomainError::invalid("flatten levels must be 0 to 3"));
        }
        if self.bandwidth_limit_kbps == Some(0) {
            return Err(DomainError::invalid(
                "a bandwidth limit must be greater than zero",
            ));
        }
        if self.event_settle_secs > MAX_EVENT_SETTLE_SECS {
            return Err(DomainError::invalid(
                "event settle time must be at most 600 seconds",
            ));
        }
        let (reacts, deletes) = match self.trigger {
            ReplicationTrigger::Event { on_push, on_delete } => (on_push || on_delete, on_delete),
            ReplicationTrigger::Manual | ReplicationTrigger::Scheduled => (true, false),
        };
        if !reacts {
            return Err(DomainError::invalid(
                "an event rule must react to pushes or deletes",
            ));
        }
        if self.replicate_deletion && !deletes {
            return Err(DomainError::invalid(
                "only an event rule that reacts to deletes can replicate deletions",
            ));
        }
        self.filter.validate()
    }
}

/// State of an execution of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionStatus {
    /// Waiting for workers.
    Queued,
    /// Tasks are running.
    Running,
    /// Every task succeeded or was skipped.
    Succeeded,
    /// Some tasks failed.
    PartiallyFailed,
    /// Every task failed.
    Failed,
    /// Stopped on request.
    Stopped,
}

impl ExecutionStatus {
    /// True when the execution is over. Only `Queued` and `Running` are active (the unique
    /// index allows one active execution per rule).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// Task counters of an execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionCounts {
    /// All tasks.
    pub total: u32,
    /// Succeeded tasks.
    pub succeeded: u32,
    /// Failed tasks.
    pub failed: u32,
    /// Skipped tasks.
    pub skipped: u32,
    /// Tasks not finished yet.
    pub in_progress: u32,
}

/// One execution of a rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReplicationExecution {
    /// Identifier.
    pub id: ReplicationExecutionId,
    /// The rule.
    pub rule_id: ReplicationRuleId,
    /// What started it.
    pub trigger: TaskTrigger,
    /// State.
    pub status: ExecutionStatus,
    /// Task counters.
    pub counts: ExecutionCounts,
    /// Set by a stop request.
    pub stop_requested: bool,
    /// Who started it.
    pub requested_by: Option<UserId>,
    /// Start time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// End time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl ReplicationExecution {
    /// A queued execution with zero counters.
    #[must_use]
    pub fn new(
        id: ReplicationExecutionId,
        rule_id: ReplicationRuleId,
        trigger: TaskTrigger,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            rule_id,
            trigger,
            status: ExecutionStatus::Queued,
            counts: ExecutionCounts::default(),
            stop_requested: false,
            requested_by: None,
            started_at: None,
            finished_at: None,
            created_at: now,
        }
    }
}

/// What a replication task does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationTaskKind {
    /// Copy a subject.
    Copy,
    /// Copy the referrers (signatures, SBOMs) of an already copied subject.
    CopyReferrers,
    /// Delete the subject at the destination.
    Delete,
}

/// State of a replication task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationTaskStatus {
    /// Waiting for a worker.
    Pending,
    /// A worker holds the lease.
    Running,
    /// Done.
    Succeeded,
    /// Failed after its attempts.
    Failed,
    /// Not done on purpose (already present, blocked by policy).
    Skipped,
    /// Stopped with its execution.
    Stopped,
}

impl ReplicationTaskStatus {
    /// True when the task will not run again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

/// One copy, referrer copy or delete of one reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReplicationTask {
    /// Identifier.
    pub id: ReplicationTaskId,
    /// The execution.
    pub execution_id: ReplicationExecutionId,
    /// Copy, copy referrers or delete.
    pub kind: ReplicationTaskKind,
    /// Local repository name.
    pub repository: String,
    /// Image path.
    pub image: String,
    /// Tag or digest.
    pub reference: String,
    /// State.
    pub status: ReplicationTaskStatus,
    /// Attempts made so far (at most 3).
    pub attempts: u8,
    /// Bytes moved so far.
    pub bytes_transferred: u64,
    /// Short stable code (`immutable`, `quota`, `blocked_by_gate`, `remote_error`, `timeout`,
    /// `digest_mismatch`, ...), never remote text, at most 64 characters.
    pub error: Option<String>,
    /// Earliest start (the settle time of an event rule, or the retry delay).
    #[serde(with = "crate::rfc3339")]
    pub available_at: OffsetDateTime,
    /// Start time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// End time.
    #[serde(default, with = "crate::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

impl ReplicationTask {
    /// A pending task with no attempts, available at `available_at`.
    #[must_use]
    pub fn new(
        id: ReplicationTaskId,
        execution_id: ReplicationExecutionId,
        kind: ReplicationTaskKind,
        repository: impl Into<String>,
        image: impl Into<String>,
        reference: impl Into<String>,
        available_at: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            execution_id,
            kind,
            repository: repository.into(),
            image: image.into(),
            reference: reference.into(),
            status: ReplicationTaskStatus::Pending,
            attempts: 0,
            bytes_transferred: 0,
            error: None,
            available_at,
            started_at: None,
            finished_at: None,
        }
    }

    /// Checks that the error is a short code.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when the error exceeds 64 characters.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self
            .error
            .as_ref()
            .is_some_and(|e| e.chars().count() > MAX_TASK_ERROR_CHARS)
        {
            return Err(DomainError::invalid(
                "a task error must be a code of at most 64 characters",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    fn rule() -> ReplicationRule {
        ReplicationRule::new(
            ReplicationRuleId::generate(),
            "to-dr",
            ReplicationDirection::Push,
            RegistryId::generate(),
            ReplicationScope::Repository(RepositoryId::generate()),
            at(),
        )
    }

    #[test]
    fn rule_constructor_sets_the_documented_defaults() {
        let rule = rule();
        assert_eq!(rule.trigger, ReplicationTrigger::Manual);
        assert!(rule.copy_referrers && rule.enabled);
        assert!(!rule.replicate_deletion && !rule.override_existing);
        assert_eq!(
            (rule.flatten_levels, rule.event_settle_secs, rule.version),
            (0, 30, 1)
        );
        assert!(rule.bandwidth_limit_kbps.is_none() && rule.schedule_task_id.is_none());
        assert!(rule.validate().is_ok());
    }

    #[test]
    fn rule_limits_follow_the_table_constraints() {
        let mut r = rule();
        r.name = String::new();
        assert!(r.validate().is_err());
        r.name = "n".repeat(129);
        assert!(r.validate().is_err());
        r.name = "n".repeat(128);
        r.remote_namespace = "x".repeat(256);
        assert!(r.validate().is_err());
        r.remote_namespace = "x".repeat(255);
        assert!(r.validate().is_ok());
        r.flatten_levels = 4;
        assert!(r.validate().is_err());
        r.flatten_levels = 3;
        r.bandwidth_limit_kbps = Some(0);
        assert!(r.validate().is_err());
        r.bandwidth_limit_kbps = Some(1);
        r.event_settle_secs = 601;
        assert!(r.validate().is_err());
        r.event_settle_secs = 600;
        assert!(r.validate().is_ok());
    }

    #[test]
    fn must_not_replicate_deletions_unless_an_event_rule_reacts_to_deletes() {
        let mut r = rule();
        r.replicate_deletion = true;
        assert!(r.validate().is_err(), "manual");
        r.trigger = ReplicationTrigger::Scheduled;
        assert!(r.validate().is_err(), "scheduled");
        r.trigger = ReplicationTrigger::Event {
            on_push: true,
            on_delete: false,
        };
        assert!(r.validate().is_err(), "an event rule that ignores deletes");
        r.trigger = ReplicationTrigger::Event {
            on_push: false,
            on_delete: true,
        };
        assert!(r.validate().is_ok());
        r.replicate_deletion = false;
        assert!(r.validate().is_ok());
    }

    #[test]
    fn an_event_rule_must_react_to_something() {
        let mut r = rule();
        r.trigger = ReplicationTrigger::Event {
            on_push: false,
            on_delete: false,
        };
        assert!(r.validate().is_err());
        r.trigger = ReplicationTrigger::Event {
            on_push: true,
            on_delete: false,
        };
        assert!(r.validate().is_ok());
    }

    #[test]
    fn filter_globs_are_validated() {
        let mut r = rule();
        r.filter.name_globs = vec!["team/**".into(), "{a,b}/*".into()];
        r.filter.tag_globs = vec!["v?.*".into()];
        assert!(r.validate().is_ok());
        r.filter.tag_globs = vec!["{".into()];
        assert!(r.validate().is_err());
        r.filter.tag_globs = Vec::new();
        r.filter.name_globs = (0..17).map(|i| format!("a{i}*")).collect();
        assert!(r.validate().is_err());
    }

    #[test]
    fn filter_reads_an_empty_object() {
        let filter: ReplicationFilter = serde_json::from_str("{}").expect("defaults");
        assert!(
            filter.name_globs.is_empty()
                && filter.label_ids.is_empty()
                && filter.resource.is_none()
        );
    }

    #[test]
    fn triggers_scopes_and_directions_use_the_documented_tags() {
        assert_wire(&ReplicationTrigger::Manual, r#"{"mode":"manual"}"#);
        assert_wire(&ReplicationTrigger::Scheduled, r#"{"mode":"scheduled"}"#);
        assert_wire(
            &ReplicationTrigger::Event {
                on_push: true,
                on_delete: false,
            },
            r#"{"mode":"event","on_push":true,"on_delete":false}"#,
        );
        let project = ProjectId::generate();
        assert_wire(
            &ReplicationScope::Project(project),
            &format!(r#"{{"kind":"project","id":"{project}"}}"#),
        );
        assert_wire(&ReplicationDirection::Pull, "\"pull\"");
        assert_wire(&ResourceFilter::Chart, "\"chart\"");
    }

    #[test]
    fn rule_serializes_rfc3339() {
        let json = round_trip_json(&rule());
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["direction"], "push");
        assert_eq!(json["trigger"]["mode"], "manual");
    }

    /// No wildcard arm: a new state has to decide whether it is terminal.
    fn expected_terminal(status: ExecutionStatus) -> (&'static str, bool) {
        match status {
            ExecutionStatus::Queued => ("queued", false),
            ExecutionStatus::Running => ("running", false),
            ExecutionStatus::Succeeded => ("succeeded", true),
            ExecutionStatus::PartiallyFailed => ("partially_failed", true),
            ExecutionStatus::Failed => ("failed", true),
            ExecutionStatus::Stopped => ("stopped", true),
        }
    }

    #[test]
    fn execution_states_match_the_database_check() {
        for status in [
            ExecutionStatus::Queued,
            ExecutionStatus::Running,
            ExecutionStatus::Succeeded,
            ExecutionStatus::PartiallyFailed,
            ExecutionStatus::Failed,
            ExecutionStatus::Stopped,
        ] {
            let (name, terminal) = expected_terminal(status);
            assert_wire(&status, &format!("\"{name}\""));
            assert_eq!(status.is_terminal(), terminal);
        }
    }

    #[test]
    fn task_states_and_kinds_match_the_database_checks() {
        for (status, name, terminal) in [
            (ReplicationTaskStatus::Pending, "pending", false),
            (ReplicationTaskStatus::Running, "running", false),
            (ReplicationTaskStatus::Succeeded, "succeeded", true),
            (ReplicationTaskStatus::Failed, "failed", true),
            (ReplicationTaskStatus::Skipped, "skipped", true),
            (ReplicationTaskStatus::Stopped, "stopped", true),
        ] {
            assert_wire(&status, &format!("\"{name}\""));
            assert_eq!(status.is_terminal(), terminal);
            match status {
                ReplicationTaskStatus::Pending
                | ReplicationTaskStatus::Running
                | ReplicationTaskStatus::Succeeded
                | ReplicationTaskStatus::Failed
                | ReplicationTaskStatus::Skipped
                | ReplicationTaskStatus::Stopped => {}
            }
        }
        assert_wire(&ReplicationTaskKind::CopyReferrers, "\"copy_referrers\"");
        assert_wire(&ReplicationTaskKind::Delete, "\"delete\"");
    }

    #[test]
    fn execution_and_task_constructors() {
        let execution = ReplicationExecution::new(
            ReplicationExecutionId::generate(),
            ReplicationRuleId::generate(),
            TaskTrigger::Manual,
            at(),
        );
        assert_eq!(execution.status, ExecutionStatus::Queued);
        assert!(!execution.stop_requested && execution.counts.total == 0);
        assert_eq!(
            round_trip_json(&execution)["created_at"],
            "2026-10-24T10:00:00Z"
        );

        let mut task = ReplicationTask::new(
            ReplicationTaskId::generate(),
            execution.id,
            ReplicationTaskKind::Copy,
            "app",
            "team/web",
            "1.0",
            at(),
        );
        assert_eq!(
            (task.status, task.attempts, task.bytes_transferred),
            (ReplicationTaskStatus::Pending, 0, 0)
        );
        assert!(task.validate().is_ok());
        task.error = Some("e".repeat(64));
        assert!(task.validate().is_ok());
        task.error = Some("e".repeat(65));
        assert!(
            task.validate().is_err(),
            "remote text does not fit a code column"
        );
        let json = round_trip_json(&task);
        assert_eq!(json["available_at"], "2026-10-24T10:00:00Z");
        assert_eq!(json["kind"], "copy");
    }
}
