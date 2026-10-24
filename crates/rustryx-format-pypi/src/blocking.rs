//! The blocking pool for work that parses or renders a whole index.
//!
//! Index pages are the only data this format buffers. Rendering or parsing one can take
//! milliseconds to seconds for a project with thousands of files, so it must not run on a
//! runtime worker. The rule of the project (README 8.1) is that the permit is taken BEFORE the
//! task is spawned and that no permit within the wait means 503 with `Retry-After`: the queue
//! of blocked work does not grow without bound.
//!
//! `FormatContext` does not expose the engine's blocking semaphore (it is private to
//! format-core), so this crate owns one; its size is [`crate::PypiConfig::blocking_permits`].

use std::sync::Arc;
use std::time::Duration;

use rustryx_format_core::FormatError;
use tokio::sync::Semaphore;

/// A bounded executor of blocking closures.
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

    /// Runs `work` on the blocking pool once a permit is free.
    ///
    /// Cancel safe: before the spawn nothing is held but the wait; after it the closure runs to
    /// the end and its result is dropped.
    pub(crate) async fn run<T, F>(&self, work: F) -> Result<T, FormatError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, FormatError> + Send + 'static,
    {
        let permit = tokio::time::timeout(self.wait, Arc::clone(&self.permits).acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or(FormatError::Unavailable {
                reason: "the server is busy building indexes, retry",
                retry_after: Some(Duration::from_secs(1)),
            })?;
        tokio::task::spawn_blocking(move || {
            // Held until the closure ends, whatever it returns.
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| FormatError::internal(format!("index task failed: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn runs_work_and_returns_its_result() {
        let pool = BlockingPool::new(2, Duration::from_secs(1));
        assert_eq!(pool.run(|| Ok(21 * 2)).await.unwrap(), 42);
        let error = pool
            .run::<(), _>(|| Err(FormatError::unprocessable("bad")))
            .await
            .unwrap_err();
        assert!(matches!(error, FormatError::Unprocessable(_)));
    }

    #[tokio::test]
    async fn a_panicking_task_is_an_internal_error_and_frees_its_permit() {
        let pool = BlockingPool::new(1, Duration::from_secs(1));
        let error = pool.run::<(), _>(|| panic!("boom")).await.unwrap_err();
        assert!(matches!(error, FormatError::Internal(_)));
        assert_eq!(pool.run(|| Ok(1)).await.unwrap(), 1);
    }

    // A paused clock does not advance while a task is blocked in `spawn_blocking`,
    // so the permit timeout would never fire.
    #[tokio::test]
    async fn must_not_queue_without_bound_when_no_permit_frees_up() {
        let pool = BlockingPool::new(1, Duration::from_millis(50));
        let started = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(std::sync::Barrier::new(2));
        let busy = {
            let (pool, started, release) =
                (pool.clone(), Arc::clone(&started), Arc::clone(&release));
            tokio::spawn(async move {
                pool.run(move || {
                    started.fetch_add(1, Ordering::SeqCst);
                    release.wait();
                    Ok(())
                })
                .await
            })
        };
        while started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let refused = pool.run(|| Ok(())).await.unwrap_err();
        assert!(matches!(
            refused,
            FormatError::Unavailable {
                retry_after: Some(_),
                ..
            }
        ));
        release.wait();
        busy.await.unwrap().unwrap();
        assert!(pool.run(|| Ok(())).await.is_ok());
    }
}
