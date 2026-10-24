//! `tasks`, `outbox` and `downloads` sections: the wave 2 platform foundation.
//!
//! These are bootstrap values: how many task runs a node starts, how long history is
//! kept, how the outbox is read. Operator tunable policy (schedules, retention rules)
//! lives in database settings; the split is ADR-W2-24 of `docs/wave2-design.md`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// `tasks` section: the background task framework.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TasksConfig {
    /// Task runs one node executes at once.
    pub max_concurrent_runs: u32,
    /// Seconds between looks at the schedule for due tasks.
    pub scheduler_tick_secs: u64,
    /// Days a finished run stays in the history.
    pub run_retention_days: u32,
    /// Days the log of a run is kept (a log cascades from its run, so never longer than
    /// `run_retention_days`).
    pub log_retention_days: u32,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            max_concurrent_runs: 4,
            scheduler_tick_secs: 15,
            run_retention_days: 90,
            log_retention_days: 30,
        }
    }
}

impl TasksConfig {
    /// Scheduler tick as a duration.
    #[must_use]
    pub fn scheduler_tick(&self) -> Duration {
        Duration::from_secs(self.scheduler_tick_secs)
    }
}

/// `outbox` section: the event outbox read by webhooks, mail and replication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OutboxConfig {
    /// Days delivered events stay before their partition is dropped.
    pub retention_days: u32,
    /// Milliseconds between polls of the outbox by a consumer.
    pub poll_interval_ms: u64,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            retention_days: 30,
            poll_interval_ms: 1000,
        }
    }
}

impl OutboxConfig {
    /// Poll interval as a duration.
    #[must_use]
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }
}

/// `downloads` section: download counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DownloadsConfig {
    /// Seconds between flushes of the in-memory counters to PostgreSQL.
    pub flush_interval_secs: u64,
    /// Days of daily counters kept. At least 35 so that a monthly view is complete.
    pub retention_days: u32,
}

impl Default for DownloadsConfig {
    fn default() -> Self {
        Self {
            flush_interval_secs: 10,
            retention_days: 90,
        }
    }
}

impl DownloadsConfig {
    /// Flush interval as a duration.
    #[must_use]
    pub fn flush_interval(&self) -> Duration {
        Duration::from_secs(self.flush_interval_secs)
    }
}
