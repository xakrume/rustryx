//! Task definitions, task runs and their run history.
//!
//! The scheduler, runner and run history live in `rustryx-tasks`. Core holds the data types and
//! the two repository ports (`TaskDefinitionRepo`, `TaskRunRepo`), so any crate can enqueue or
//! read runs. The `TaskHandler` trait is not here: it carries a cancellation token, and core has
//! no dependency that provides one.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{TaskDefinitionId, TaskRunId};

/// When a task runs. UTC only: hourly, daily, weekly and monthly are cron shortcuts of the UI,
/// and with no time zone there is no daylight saving ambiguity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Schedule {
    /// Only when someone starts it.
    Manual,
    /// Once, at the given time.
    Once {
        /// When to run.
        #[serde(with = "crate::rfc3339")]
        at: OffsetDateTime,
    },
    /// Every `secs` seconds.
    Every {
        /// Interval in seconds.
        secs: u32,
    },
    /// Five field cron, validated by `rustryx-tasks` (the parser is not a dependency of core).
    Cron {
        /// The expression.
        expr: String,
    },
}

/// Whether a task run sends an e-mail (`task.finished` event) when it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskNotify {
    /// Notify when a run fails.
    pub on_failure: bool,
    /// Notify when a run succeeds.
    pub on_success: bool,
}

/// A task: what to run and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskDefinition {
    /// Identifier.
    pub id: TaskDefinitionId,
    /// Key of a registered `TaskHandler` (fixed vocabulary, see the job catalog).
    pub kind: String,
    /// Unique name.
    pub name: String,
    /// Handler parameters (a JSON object).
    pub params: serde_json::Value,
    /// When it runs.
    pub schedule: Schedule,
    /// A disabled task is never scheduled.
    pub enabled: bool,
    /// System tasks are seeded, cannot be deleted or have their kind or params changed;
    /// schedule, enabled and notify are editable.
    pub system: bool,
    /// E-mail on completion.
    pub notify: TaskNotify,
    /// The next due time, maintained by the scheduler with a compare and set.
    #[serde(default, with = "crate::rfc3339::option")]
    pub next_run_at: Option<OffsetDateTime>,
    /// The most recent run.
    pub last_run_id: Option<TaskRunId>,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl TaskDefinition {
    /// An enabled, non system task with empty parameters and no notification.
    #[must_use]
    pub fn new(
        id: TaskDefinitionId,
        kind: impl Into<String>,
        name: impl Into<String>,
        schedule: Schedule,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            kind: kind.into(),
            name: name.into(),
            params: serde_json::json!({}),
            schedule,
            enabled: true,
            system: false,
            notify: TaskNotify::default(),
            next_run_at: None,
            last_run_id: None,
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }
}

/// State of a task run (also used by lifecycle, GC and verification runs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskRunStatus {
    /// Waiting for a runner.
    Queued,
    /// A runner holds the lease.
    Running,
    /// Finished without error.
    Succeeded,
    /// Finished with an error.
    Failed,
    /// Stopped on request.
    Cancelled,
    /// Not run, for example because read only mode is on.
    Skipped,
    /// The lease expired and the kind cannot resume.
    Abandoned,
}

impl TaskRunStatus {
    /// True for every state a run cannot leave again. Only `Queued` and `Running` are not
    /// terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskTrigger {
    /// Its schedule fired.
    Schedule,
    /// A user started it.
    Manual,
    /// An event started it.
    Event,
    /// The process started it at startup.
    Startup,
}

/// One run of a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskRun {
    /// Identifier.
    pub id: TaskRunId,
    /// The definition, `None` for an ad hoc run or after the definition was deleted.
    pub task_id: Option<TaskDefinitionId>,
    /// The handler kind.
    pub kind: String,
    /// Part of the singleton key: a store id, a policy id, a rule id, or empty.
    pub scope_key: String,
    /// State.
    pub status: TaskRunStatus,
    /// What started it.
    pub trigger: TaskTrigger,
    /// A dry run changes nothing.
    pub dry_run: bool,
    /// Parameters of this run.
    pub params: serde_json::Value,
    /// The node that holds or held the lease.
    pub node_id: Option<String>,
    /// When it was queued.
    #[serde(with = "crate::rfc3339")]
    pub queued_at: OffsetDateTime,
    /// When a runner started it.
    #[serde(default, with = "crate::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// When it ended.
    #[serde(default, with = "crate::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// Counts and a short text, never secrets, at most 8 KiB.
    pub summary: serde_json::Value,
    /// Short error text, at most 500 characters.
    pub error: Option<String>,
    /// Set by a stop request; the runner sees it at the next heartbeat.
    pub stop_requested: bool,
}

impl TaskRun {
    /// A queued run with an empty scope key, empty parameters and no error.
    #[must_use]
    pub fn new(
        id: TaskRunId,
        kind: impl Into<String>,
        trigger: TaskTrigger,
        queued_at: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            task_id: None,
            kind: kind.into(),
            scope_key: String::new(),
            status: TaskRunStatus::Queued,
            trigger,
            dry_run: false,
            params: serde_json::json!({}),
            node_id: None,
            queued_at,
            started_at: None,
            finished_at: None,
            summary: serde_json::json!({}),
            error: None,
            stop_requested: false,
        }
    }
}

/// Input of `TaskRunRepo::enqueue`.
#[derive(Debug, Clone)]
pub struct NewTaskRun {
    /// The definition, if any.
    pub task_id: Option<TaskDefinitionId>,
    /// The handler kind.
    pub kind: String,
    /// Part of the singleton key.
    pub scope_key: String,
    /// What starts it.
    pub trigger: TaskTrigger,
    /// A dry run changes nothing.
    pub dry_run: bool,
    /// Parameters.
    pub params: serde_json::Value,
}

/// One line of a run's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLogLine {
    /// Position in the log.
    pub seq: i32,
    /// When the line was written.
    pub at: OffsetDateTime,
    /// Severity as a small number (as stored).
    pub level: u8,
    /// The text, at most 1024 characters.
    pub message: String,
}

/// How a run ended, handed to `TaskRunRepo::finish`.
#[derive(Debug, Clone)]
pub struct TaskOutcome {
    /// The terminal state.
    pub status: TaskRunStatus,
    /// Counts and a short text, never secrets.
    pub summary: serde_json::Value,
    /// Short error text.
    pub error: Option<String>,
}

/// Answer of a run heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeat {
    /// False once the lease was lost: stop and write nothing more.
    pub owned: bool,
    /// True when a stop was requested: cancel at the next safe point.
    pub stop_requested: bool,
}

/// What the orphan reaper did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reaped {
    /// Runs of resumable kinds put back in the queue.
    pub requeued: u64,
    /// Runs marked abandoned.
    pub abandoned: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{assert_wire, at, round_trip_json};

    /// No wildcard arm: a new state forces a decision about its wire name and whether it ends
    /// a run.
    fn expected(status: TaskRunStatus) -> (&'static str, bool) {
        match status {
            TaskRunStatus::Queued => ("queued", false),
            TaskRunStatus::Running => ("running", false),
            TaskRunStatus::Succeeded => ("succeeded", true),
            TaskRunStatus::Failed => ("failed", true),
            TaskRunStatus::Cancelled => ("cancelled", true),
            TaskRunStatus::Skipped => ("skipped", true),
            TaskRunStatus::Abandoned => ("abandoned", true),
        }
    }

    #[test]
    fn run_status_wire_names_match_the_database_check_and_terminal_states() {
        let all = [
            TaskRunStatus::Queued,
            TaskRunStatus::Running,
            TaskRunStatus::Succeeded,
            TaskRunStatus::Failed,
            TaskRunStatus::Cancelled,
            TaskRunStatus::Skipped,
            TaskRunStatus::Abandoned,
        ];
        for status in all {
            let (name, terminal) = expected(status);
            assert_wire(&status, &format!("\"{name}\""));
            assert_eq!(status.is_terminal(), terminal, "{status:?}");
        }
        // The `task_runs.status` CHECK list of migration 0007, in the same order.
        let names: Vec<_> = all.iter().map(|s| expected(*s).0).collect();
        assert_eq!(
            names,
            [
                "queued",
                "running",
                "succeeded",
                "failed",
                "cancelled",
                "skipped",
                "abandoned"
            ]
        );
    }

    #[test]
    fn trigger_wire_names_match_the_database_check() {
        for (trigger, name) in [
            (TaskTrigger::Schedule, "schedule"),
            (TaskTrigger::Manual, "manual"),
            (TaskTrigger::Event, "event"),
            (TaskTrigger::Startup, "startup"),
        ] {
            assert_wire(&trigger, &format!("\"{name}\""));
        }
    }

    #[test]
    fn schedules_are_internally_tagged_and_utc() {
        assert_wire(&Schedule::Manual, r#"{"type":"manual"}"#);
        assert_wire(
            &Schedule::Every { secs: 300 },
            r#"{"type":"every","secs":300}"#,
        );
        assert_wire(
            &Schedule::Cron {
                expr: "0 3 * * 0".into(),
            },
            r#"{"type":"cron","expr":"0 3 * * 0"}"#,
        );
        assert_wire(
            &Schedule::Once { at: at() },
            r#"{"type":"once","at":"2026-10-24T10:00:00Z"}"#,
        );
    }

    #[test]
    fn notify_defaults_to_silent() {
        assert_eq!(
            TaskNotify::default(),
            TaskNotify {
                on_failure: false,
                on_success: false
            }
        );
        assert_wire(
            &TaskNotify::default(),
            r#"{"on_failure":false,"on_success":false}"#,
        );
    }

    #[test]
    fn definition_constructor_defaults() {
        let task = TaskDefinition::new(
            TaskDefinitionId::generate(),
            "blob_gc",
            "gc default",
            Schedule::Every { secs: 3600 },
            at(),
        );
        assert!(task.enabled && !task.system);
        assert_eq!(task.version, 1);
        assert!(
            task.params
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
        assert!(task.next_run_at.is_none() && task.last_run_id.is_none());
        let mut scheduled = task;
        scheduled.next_run_at = Some(at() + time::Duration::hours(1));
        let json = round_trip_json(&scheduled);
        assert_eq!(json["next_run_at"], "2026-10-24T11:00:00Z");
        assert_eq!(json["schedule"]["type"], "every");
        assert_eq!(json["created_at"], "2026-10-24T10:00:00Z");
    }

    #[test]
    fn run_constructor_defaults() {
        let run = TaskRun::new(TaskRunId::generate(), "cleanup", TaskTrigger::Manual, at());
        assert_eq!(run.status, TaskRunStatus::Queued);
        assert!(run.scope_key.is_empty() && !run.dry_run && !run.stop_requested);
        assert!(run.node_id.is_none() && run.error.is_none());
        let json = round_trip_json(&run);
        assert_eq!(json["queued_at"], "2026-10-24T10:00:00Z");
        assert!(json["started_at"].is_null());
        assert_eq!(json["trigger"], "manual");
    }
}
