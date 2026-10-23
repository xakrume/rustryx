//! Supervision of long-lived background tasks.
//!
//! Every background task of the process (metrics upkeep today, schedulers and
//! garbage collection once wired) runs in one [`JoinSet`] under a shared
//! [`CancellationToken`]. A task that panics or returns before shutdown is logged,
//! counted and restarted with exponential back-off; a bare `tokio::spawn` whose
//! handle nobody keeps would instead die silently, which is the failure the
//! health probes cannot see ("process alive, job dead").

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use rustryx_observability::MetricsHandle;
use rustryx_observability::names::BACKGROUND_JOB_RUNS_TOTAL;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_mins(1);
/// A run that lasted this long counts as healthy and resets the back-off.
const HEALTHY_RUN: Duration = Duration::from_mins(1);

/// Builds one run of a task. It is a factory because a restart needs a fresh future.
pub type TaskFactory = Arc<dyn Fn(CancellationToken) -> BoxFuture<'static, ()> + Send + Sync>;

/// A named background task.
#[derive(Clone)]
pub struct BackgroundTask {
    /// Name for logs and the `job` metric label (a fixed vocabulary, never user input).
    pub name: &'static str,
    make: TaskFactory,
}

impl BackgroundTask {
    /// A task whose body returns when the token is cancelled.
    pub fn new<F, Fut>(name: &'static str, body: F) -> Self
    where
        F: Fn(CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self {
            name,
            make: Arc::new(move |cancel| body(cancel).boxed()),
        }
    }
}

impl std::fmt::Debug for BackgroundTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundTask")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Outcome of [`Supervisor::shutdown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopReport {
    /// Tasks that returned within the deadline.
    pub stopped: usize,
    /// Tasks that had to be aborted at the deadline.
    pub aborted: usize,
}

/// Owns the join set of background tasks.
pub struct Supervisor {
    tasks: JoinSet<()>,
    cancel: CancellationToken,
    metrics: Option<MetricsHandle>,
}

impl Supervisor {
    /// A supervisor whose tasks stop when `cancel` fires.
    #[must_use]
    pub fn new(cancel: CancellationToken, metrics: Option<MetricsHandle>) -> Self {
        Self {
            tasks: JoinSet::new(),
            cancel,
            metrics,
        }
    }

    /// Starts a task under supervision.
    pub fn spawn(&mut self, task: BackgroundTask) {
        let cancel = self.cancel.clone();
        let metrics = self.metrics.clone();
        self.tasks.spawn(run_supervised(task, cancel, metrics));
    }

    /// Number of supervised tasks still running.
    #[must_use]
    pub fn running(&self) -> usize {
        self.tasks.len()
    }

    /// Cancels the shared token and waits up to `deadline` for the tasks, then aborts
    /// the rest. Tasks are expected to stop at a safe point; an aborted one leaves state
    /// that the next run picks up (tasks are idempotent).
    pub async fn shutdown(mut self, deadline: Duration) -> StopReport {
        self.cancel.cancel();
        let total = self.tasks.len();
        let wait = async { while self.tasks.join_next().await.is_some() {} };
        let finished = tokio::time::timeout(deadline, wait).await.is_ok();
        let aborted = if finished { 0 } else { self.tasks.len() };
        if !finished {
            self.tasks.abort_all();
            while self.tasks.join_next().await.is_some() {}
        }
        StopReport {
            stopped: total - aborted,
            aborted,
        }
    }
}

async fn run_supervised(
    task: BackgroundTask,
    cancel: CancellationToken,
    metrics: Option<MetricsHandle>,
) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let started = Instant::now();
        let run = AssertUnwindSafe((task.make)(cancel.clone()))
            .catch_unwind()
            .await;
        if cancel.is_cancelled() {
            return;
        }
        let result = if run.is_ok() {
            tracing::error!(
                job = task.name,
                "background task exited before shutdown, restarting"
            );
            "exit"
        } else {
            tracing::error!(job = task.name, "background task panicked, restarting");
            "panic"
        };
        if let Some(metrics) = &metrics {
            metrics
                .counter(
                    BACKGROUND_JOB_RUNS_TOTAL,
                    &[("job", task.name), ("result", result)],
                )
                .increment(1);
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = if started.elapsed() >= HEALTHY_RUN {
            INITIAL_BACKOFF
        } else {
            (backoff * 2).min(MAX_BACKOFF)
        };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_task_that_returns_cancel_stops_cleanly() {
        let cancel = CancellationToken::new();
        let mut supervisor = Supervisor::new(cancel.clone(), None);
        supervisor.spawn(BackgroundTask::new("waits", |cancel| async move {
            cancel.cancelled().await;
        }));
        assert_eq!(supervisor.running(), 1);
        let report = supervisor.shutdown(Duration::from_secs(5)).await;
        assert_eq!(
            report,
            StopReport {
                stopped: 1,
                aborted: 0
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_panicking_task_is_restarted_with_growing_back_off_and_counted() {
        let runs = Arc::new(AtomicUsize::new(0));
        let metrics = MetricsHandle::new().expect("metrics");
        let mut supervisor = Supervisor::new(CancellationToken::new(), Some(metrics.clone()));
        let counter = runs.clone();
        supervisor.spawn(BackgroundTask::new("flaky", move |_cancel| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                panic!("job blew up");
            }
        }));
        // Back-off 1 s, 2 s, 4 s: after 8 virtual seconds there were 4 runs.
        tokio::time::sleep(Duration::from_secs(8)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 4);
        let text = metrics.render();
        assert!(
            text.contains(r#"background_job_runs_total{job="flaky",result="panic"} 4"#),
            "{text}"
        );
        let report = supervisor.shutdown(Duration::from_secs(1)).await;
        assert_eq!(report.aborted, 0, "a sleeping supervisor stops at cancel");
    }

    #[tokio::test(start_paused = true)]
    async fn a_task_that_exits_early_is_restarted() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut supervisor = Supervisor::new(CancellationToken::new(), None);
        let counter = runs.clone();
        supervisor.spawn(BackgroundTask::new("quitter", move |_cancel| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }));
        tokio::time::sleep(Duration::from_millis(3500)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 3, "runs at 0 s, 1 s and 3 s");
        let _ = supervisor.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_task_that_ignores_cancellation_is_aborted_at_the_deadline() {
        let mut supervisor = Supervisor::new(CancellationToken::new(), None);
        supervisor.spawn(BackgroundTask::new("stubborn", |_cancel| async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let report = supervisor.shutdown(Duration::from_secs(2)).await;
        assert_eq!(
            report,
            StopReport {
                stopped: 0,
                aborted: 1
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_task_is_restarted_after_shutdown_began() {
        let runs = Arc::new(AtomicUsize::new(0));
        let cancel = CancellationToken::new();
        let mut supervisor = Supervisor::new(cancel.clone(), None);
        let counter = runs.clone();
        supervisor.spawn(BackgroundTask::new("once", move |_cancel| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }));
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
        let _ = supervisor.shutdown(Duration::from_secs(1)).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn debug_shows_the_name_only() {
        let task = BackgroundTask::new("x", |_c| async {});
        assert!(format!("{task:?}").contains("\"x\""));
    }
}
