//! Steps that run once, after the listener has drained and the background tasks have stopped.
//!
//! The order matters and is the order of the list:
//!
//! 1. **The format engine** stops its cache fills (its token is cancelled), lets the commit
//!    phases of uploads finish and flushes the download counters (`FormatEngine::drain`).
//! 2. **The blob stores** wait for the cleanup of dropped writers.
//! 3. **The database pool** closes last, because the steps above still write through it.
//!
//! Each step gets what is left of one shared budget. A step that does not finish in time is
//! abandoned with a warning; the process exits anyway, because a node that cannot stop is worse
//! for a rolling update than one that loses the tail of a statistic.

use std::future::Future;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::time::Instant;

/// A named step of the final shutdown.
pub struct Finalizer {
    name: &'static str,
    run: BoxFuture<'static, ()>,
}

impl Finalizer {
    /// A step. The future is not polled before [`run_all`] reaches it.
    pub fn new(name: &'static str, step: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            name,
            run: step.boxed(),
        }
    }

    /// The name in logs.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl std::fmt::Debug for Finalizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Finalizer")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// What [`run_all`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizeReport {
    /// Steps that finished within the budget.
    pub finished: usize,
    /// Steps that were abandoned at the deadline.
    pub abandoned: usize,
}

/// Runs the steps in order within one shared `budget`.
pub async fn run_all(finalizers: Vec<Finalizer>, budget: Duration) -> FinalizeReport {
    let deadline = Instant::now() + budget;
    let mut report = FinalizeReport {
        finished: 0,
        abandoned: 0,
    };
    for step in finalizers {
        let name = step.name;
        if tokio::time::timeout_at(deadline, step.run).await.is_ok() {
            tracing::debug!(step = name, "shutdown step finished");
            report.finished += 1;
        } else {
            tracing::warn!(
                step = name,
                "shutdown step did not finish in time and was abandoned"
            );
            report.abandoned += 1;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn steps_run_in_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let steps = ["engine", "blobs", "pool"]
            .into_iter()
            .map(|name| {
                let log = Arc::clone(&log);
                Finalizer::new(name, async move { log.lock().unwrap().push(name) })
            })
            .collect();
        let report = run_all(steps, Duration::from_secs(5)).await;
        assert_eq!(
            report,
            FinalizeReport {
                finished: 3,
                abandoned: 0
            }
        );
        assert_eq!(log.lock().unwrap().as_slice(), ["engine", "blobs", "pool"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stuck_step_is_abandoned_and_the_budget_is_shared() {
        let ran_last = Arc::new(Mutex::new(false));
        let last = Arc::clone(&ran_last);
        let steps = vec![
            Finalizer::new("stuck", async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }),
            Finalizer::new("after", async move { *last.lock().unwrap() = true }),
        ];
        let started = Instant::now();
        let report = run_all(steps, Duration::from_secs(2)).await;
        assert!(started.elapsed() <= Duration::from_secs(3));
        assert!(
            report.abandoned >= 1,
            "the stuck step is cut at the deadline"
        );
        assert_eq!(report.finished + report.abandoned, 2);
    }

    #[test]
    fn debug_shows_the_name_only() {
        let step = Finalizer::new("x", async {});
        assert!(format!("{step:?}").contains("\"x\""));
        assert_eq!(step.name(), "x");
    }
}
