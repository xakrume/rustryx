//! Graceful shutdown support shared by both stores.

use async_trait::async_trait;

/// Waits for the background cleanup that dropped writers leave behind: removal of an
/// abandoned temporary file (local) or abort of an abandoned multipart upload (S3).
///
/// `Drop` cannot await, so it spawns that work on a task tracker owned by the store.
/// A server calls this after in-flight requests have been drained (architecture 1.4:
/// unfinished multipart uploads are aborted before exit) so the process does not
/// exit with cleanup still queued.
#[async_trait]
pub trait CleanupDrain: Send + Sync + 'static {
    /// Resolves when no cleanup task is running. Cancel safe; safe to call
    /// repeatedly and concurrently.
    async fn drain_cleanup(&self);
}
