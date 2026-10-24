//! Chart parsing runs on the blocking pool. A permit is taken before the spawn, and the wait
//! is bounded, so a flood of uploads cannot fill the pool.

use std::sync::Arc;
use std::time::Duration;

use rustryx_format_core::FormatError;
use tokio::sync::Semaphore;

/// How many chart parses may run at once.
pub(crate) const PARSE_PERMITS: usize = 8;
/// How long a request waits for a parse permit before answering 503.
pub(crate) const PARSE_WAIT: Duration = Duration::from_secs(5);

/// Permits in front of `spawn_blocking`.
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

    /// Runs `work` off the async worker.
    ///
    /// # Errors
    /// `Unavailable` when no permit is free in time, `Internal` when the task panics.
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
                reason: "the server is busy reading a chart, retry",
                retry_after: Some(Duration::from_secs(1)),
            })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| FormatError::internal(format!("chart task failed: {error}")))
    }
}
