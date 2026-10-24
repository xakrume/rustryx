//! Parsing of documents on the blocking pool, behind a permit.
//!
//! A packument can be tens of megabytes and `serde_json` parses it synchronously: on a runtime
//! worker that stalls every other request of the node. The work goes to `spawn_blocking`, and the
//! permit is taken before the spawn, so a burst of large documents is a queue of waiting callers
//! that gives up with `503` and `Retry-After`, not an unbounded pile of blocking threads.
//!
//! `rustryx-format-core` has a permit pool of its own for index merges, but it is private to the
//! engine; a format that parses documents in `handle` needs a pool it owns (reported as a gap in
//! `docs/format-npm.md`).

use std::sync::Arc;
use std::time::Duration;

use rustryx_format_core::FormatError;
use tokio::sync::Semaphore;

/// A bounded pool of permits for blocking work.
#[derive(Debug, Clone)]
pub struct Blocking {
    permits: Arc<Semaphore>,
}

impl Blocking {
    /// A pool with `permits` permits (at least one).
    #[must_use]
    pub fn new(permits: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(permits.max(1))),
        }
    }

    /// Permits that are free right now.
    #[must_use]
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    /// Runs `work` on the blocking pool.
    ///
    /// Waits at most `wait` for a permit; without one the caller gets
    /// [`FormatError::Unavailable`], which renders as `503` with `Retry-After`.
    ///
    /// Cancel safe: dropping the future before the work starts releases the permit; work that
    /// already started finishes and its result is dropped.
    ///
    /// # Errors
    /// `Unavailable` without a permit in time, `Internal` when the work panicked.
    pub async fn run<T, F>(&self, wait: Duration, work: F) -> Result<T, FormatError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let permit = tokio::time::timeout(wait, Arc::clone(&self.permits).acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or(FormatError::Unavailable {
                reason: "the server is busy parsing package documents, retry",
                retry_after: Some(Duration::from_secs(1)),
            })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| FormatError::internal(format!("document task failed: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_work_and_returns_its_result() {
        let pool = Blocking::new(2);
        let answer = pool.run(Duration::from_secs(1), || 6 * 7).await.unwrap();
        assert_eq!(answer, 42);
        assert_eq!(pool.available(), 2, "the permit is released");
    }

    #[tokio::test]
    async fn must_not_queue_without_a_permit() {
        let pool = Blocking::new(1);
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let pool = pool.clone();
            tokio::spawn(async move {
                pool.run(Duration::from_secs(5), move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
                .await
            })
        };
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        let refused = pool.run(Duration::from_millis(20), || ()).await;
        assert!(matches!(refused, Err(FormatError::Unavailable { .. })));
        release_tx.send(()).unwrap();
        holder.await.unwrap().unwrap();
        pool.run(Duration::from_secs(1), || ()).await.unwrap();
    }

    #[tokio::test]
    async fn a_panicking_job_is_an_error_not_a_crash() {
        let pool = Blocking::new(1);
        let result = pool
            .run(Duration::from_secs(1), || -> u8 { panic!("boom") })
            .await;
        assert!(matches!(result, Err(FormatError::Internal(_))));
        assert_eq!(pool.available(), 1);
    }
}
