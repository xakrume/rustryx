//! Housekeeping of the database that nothing else does.
//!
//! One node does it at a time, chosen by a lock on the direct connection; the others find the
//! lock taken and skip. What runs, and what deliberately does not:
//!
//! - **Audit partitions are created ahead.** The audit table is partitioned by month, an insert
//!   into a month without a partition fails, and an audit write that fails fails the action it
//!   describes. Creating the next months is therefore not optional.
//! - **Audit partitions are NOT dropped.** `drop_expired_audit_partitions` deletes data for good,
//!   and the retention rule requires an archive first. That step belongs to whoever owns audit
//!   retention; running the drop here would delete evidence on a timer.
//! - **Expired revocations and stale login counters are purged.** A revoked token id whose `exp`
//!   passed can never be presented again, and a failure counter older than the longest lockout
//!   (30 minutes) locks nobody out; both only take space.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use rustryx_core::ports::{LockBackend, RevokedTokenRepo};
use rustryx_db::AuditMaintenance;
use rustryx_observability::{JobMetrics, JobResult, MetricsHandle};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::database::schema_is_serving;
use crate::supervisor::BackgroundTask;

/// Name of the task in metrics, and of the lock (a fixed vocabulary, never user input).
const TASK: &str = "db_maintenance";
/// Pause after a pass that worked.
const AFTER_SUCCESS: Duration = Duration::from_hours(1);
/// Pause after a pass that failed or found the schema not servable yet.
const AFTER_FAILURE: Duration = Duration::from_mins(1);
/// A login counter this old is dead weight: the longest lockout is 30 minutes.
const LOGIN_COUNTER_MAX_AGE: time::Duration = time::Duration::days(1);

/// How one pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// This node did the work.
    Done,
    /// Another node held the lock; nothing to do here.
    Skipped,
    /// The schema is not servable yet (before `rustryx migrate`).
    SchemaNotReady,
    /// The pass failed; it is logged and retried sooner.
    Failed,
}

/// What the maintenance pass needs.
pub struct Maintenance {
    /// The pool the statements run on.
    pub pool: PgPool,
    /// Revoked session token ids.
    pub revoked: Arc<dyn RevokedTokenRepo>,
    /// Picks the node that runs the pass.
    pub locks: Arc<dyn LockBackend>,
    /// How long the lock is valid without renewal. A pass is a few statements, far shorter.
    pub lock_ttl: Duration,
    /// Where the task reports.
    pub metrics: MetricsHandle,
}

impl Maintenance {
    /// The supervised task.
    #[must_use]
    pub fn task(self) -> BackgroundTask {
        let maintenance = Arc::new(self);
        BackgroundTask::new(TASK, move |cancel| {
            let maintenance = Arc::clone(&maintenance);
            async move { maintenance.run(&cancel).await }
        })
    }

    async fn run(&self, cancel: &CancellationToken) {
        let job = self.metrics.job(TASK);
        let mut pause = Duration::from_secs(10);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(pause) => {}
            }
            pause = match self.pass(&job).await {
                Pass::Done | Pass::Skipped => AFTER_SUCCESS,
                Pass::SchemaNotReady | Pass::Failed => AFTER_FAILURE,
            };
        }
    }

    /// One pass: take the lock, do the work, release.
    pub async fn pass(&self, job: &JobMetrics) -> Pass {
        if !schema_is_serving(&self.pool).await {
            return Pass::SchemaNotReady;
        }
        job.started();
        match self.locked().await {
            Ok(Some(())) => {
                job.finished(JobResult::Ok);
                Pass::Done
            }
            Ok(None) => {
                job.finished(JobResult::Skipped);
                Pass::Skipped
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "database maintenance failed");
                job.finished(JobResult::Error);
                Pass::Failed
            }
        }
    }

    async fn locked(&self) -> anyhow::Result<Option<()>> {
        let acquired = self.locks.try_acquire(TASK, self.lock_ttl).await;
        let guard = match acquired {
            Ok(Some(guard)) => guard,
            Ok(None) => {
                self.metrics.record_lock_acquire(TASK, "busy");
                return Ok(None);
            }
            Err(error) => {
                self.metrics.record_lock_acquire(TASK, "error");
                return Err(anyhow!("take the maintenance lock: {error}"));
            }
        };
        self.metrics.record_lock_acquire(TASK, "acquired");
        let result = self.work().await;
        if let Err(error) = guard.release().await {
            tracing::warn!(%error, "releasing the maintenance lock failed; it expires on its own");
        }
        result.map(Some)
    }

    async fn work(&self) -> anyhow::Result<()> {
        let now = OffsetDateTime::now_utc();
        let policy = AuditMaintenance::default();
        let created = rustryx_db::maintenance::ensure_audit_partitions(
            &self.pool,
            now,
            policy.lookahead_months,
        )
        .await
        .context("create audit partitions")?;
        let revoked = self
            .revoked
            .purge_expired(now)
            .await
            .context("purge expired revoked tokens")?;
        let counters =
            rustryx_db::maintenance::purge_login_attempts(&self.pool, now - LOGIN_COUNTER_MAX_AGE)
                .await
                .context("purge stale login counters")?;
        tracing::debug!(created, revoked, counters, "database maintenance finished");
        Ok(())
    }
}
