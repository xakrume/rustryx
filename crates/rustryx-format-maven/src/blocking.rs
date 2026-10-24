//! Parsing on the blocking pool, behind a permit.

use std::sync::Arc;
use std::time::Duration;

use rustryx_format_core::FormatError;
use tokio::sync::Semaphore;

/// A bounded pool for CPU work that must not run on a runtime worker (parsing a metadata
/// document of up to ten megabytes).
///
/// The permit is taken before `spawn_blocking`. Without a permit before the wait elapses the
/// request is refused with 503 and `Retry-After`: the queue of blocking work does not grow, which
/// is what keeps a flood of uploads from filling the blocking pool (README 8.1).
#[derive(Debug, Clone)]
pub(crate) struct BlockingPool {
    permits: Arc<Semaphore>,
    wait: Duration,
}

impl BlockingPool {
    pub(crate) fn new(permits: usize, wait: Duration) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(permits.max(1))),
            wait,
        }
    }

    /// Runs `work` on the blocking pool.
    ///
    /// # Errors
    /// `Unavailable` when no permit frees up in time, `Internal` when the task panics or is
    /// cancelled (the parsers are panic free, so this is a bug, never a client's doing).
    pub(crate) async fn run<T, F>(&self, work: F) -> Result<T, FormatError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let permit = tokio::time::timeout(self.wait, Arc::clone(&self.permits).acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or(FormatError::Unavailable {
                reason: "the server is busy parsing metadata, retry",
                retry_after: Some(Duration::from_secs(1)),
            })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| FormatError::internal(format!("metadata task failed: {error}")))
    }

    /// Permits that are free right now. For tests.
    #[cfg(test)]
    pub(crate) fn available(&self) -> usize {
        self.permits.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_work_and_returns_the_permit() {
        let pool = BlockingPool::new(2, Duration::from_millis(50));
        let value = pool.run(|| 21 * 2).await.unwrap();
        assert_eq!(value, 42);
        assert_eq!(pool.available(), 2);
    }

    #[tokio::test]
    async fn must_not_queue_when_no_permit_is_free() {
        let pool = BlockingPool::new(1, Duration::from_millis(30));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let busy = pool.clone();
        let holder = tokio::spawn(async move {
            busy.run(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
            })
            .await
        });
        // Wait on the blocking thread side, not with a sleep.
        let wait = tokio::task::spawn_blocking(move || started_rx.recv());
        wait.await.unwrap().unwrap();
        let refused = pool.run(|| 1).await;
        assert!(matches!(
            refused,
            Err(FormatError::Unavailable {
                retry_after: Some(_),
                ..
            })
        ));
        release_tx.send(()).unwrap();
        holder.await.unwrap().unwrap();
        assert_eq!(pool.run(|| 7).await.unwrap(), 7);
    }

    #[tokio::test]
    async fn a_panicking_task_is_an_internal_error_and_frees_the_permit() {
        let pool = BlockingPool::new(1, Duration::from_millis(50));
        let result: Result<(), FormatError> = pool.run(|| panic!("boom")).await;
        assert!(matches!(result, Err(FormatError::Internal(_))));
        assert_eq!(pool.available(), 1);
    }
}
