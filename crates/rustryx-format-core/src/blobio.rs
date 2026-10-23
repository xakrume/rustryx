//! Cancellation-safe streaming into a blob store.
//!
//! [`BlobSink`] is the one place that owns a `BlobWriter` during an upload or a
//! cache fill. It enforces the byte limit, feeds the writer in slices of at most
//! 64 KiB and, whatever happens to the surrounding future, makes sure the staged
//! data does not stay behind:
//!
//! * error paths call [`BlobSink::abort`] and await it;
//! * a dropped sink (the request future was cancelled, a client left) spawns the
//!   abort on the runtime, because `Drop` cannot await. The `BlobStore` contract
//!   additionally promises that dropping a writer discards its data, so even a
//!   missed abort is only an early-cleanup miss, never a visible partial blob.

use bytes::Bytes;
use rustryx_core::error::BlobError;
use rustryx_core::ports::{BlobCommit, BlobKey, BlobStore, BlobWriter};

use crate::context::FormatContext;
use crate::metrics;
use crate::stream::hash_slices;

/// After this many bytes the sink yields to the scheduler. Local, fast streams
/// (a body that is already in memory) never return `Pending`, and hashing at
/// memory speed would otherwise starve other tasks on the worker thread.
const YIELD_EVERY: usize = 1024 * 1024;

/// Failure while pushing bytes into a [`BlobSink`].
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    /// The byte limit was crossed; nothing of the offending chunk was written.
    #[error("limit of {limit} bytes exceeded")]
    LimitExceeded {
        /// The limit that was crossed.
        limit: u64,
    },
    /// The blob store failed.
    #[error(transparent)]
    Blob(#[from] BlobError),
}

/// An in-progress blob write with a byte limit and abort-on-drop.
pub struct BlobSink {
    writer: Option<Box<dyn BlobWriter>>,
    key: BlobKey,
    written: u64,
    limit: u64,
    since_yield: usize,
}

impl BlobSink {
    /// Opens a streamed write to `key`.
    ///
    /// `limit` is the most bytes that may be written; `size_hint` lets S3 pick a
    /// part size.
    ///
    /// Cancel safe: nothing is staged until `begin` returns.
    ///
    /// # Errors
    /// Whatever `BlobStore::begin` returns.
    pub async fn begin(
        store: &dyn BlobStore,
        key: BlobKey,
        size_hint: Option<u64>,
        limit: u64,
    ) -> Result<Self, BlobError> {
        let writer = store.begin(&key, size_hint).await?;
        Ok(Self {
            writer: Some(writer),
            key,
            written: 0,
            limit,
            since_yield: 0,
        })
    }

    /// The store key being written.
    #[must_use]
    pub fn key(&self) -> &BlobKey {
        &self.key
    }

    /// Bytes accepted so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Appends a chunk, splitting it into slices of at most 64 KiB.
    ///
    /// Not cancel safe for the sink: after a cancelled or failed `write` the sink
    /// must be aborted, not reused (the `BlobWriter` contract). Dropping it does that.
    ///
    /// # Errors
    /// [`SinkError::LimitExceeded`] when the chunk would cross the limit,
    /// [`SinkError::Blob`] when the writer fails or is already closed.
    pub async fn write(&mut self, chunk: Bytes) -> Result<(), SinkError> {
        let next = self.written.saturating_add(chunk.len() as u64);
        if next > self.limit {
            return Err(SinkError::LimitExceeded { limit: self.limit });
        }
        let Some(writer) = self.writer.as_mut() else {
            return Err(SinkError::Blob(BlobError::WriterClosed));
        };
        for slice in hash_slices(chunk) {
            let len = slice.len();
            writer.write(slice).await?;
            self.since_yield += len;
            if self.since_yield >= YIELD_EVERY {
                self.since_yield = 0;
                tokio::task::yield_now().await;
            }
        }
        self.written = next;
        Ok(())
    }

    /// Makes the object visible and returns size and digests.
    ///
    /// Not cancel safe: if the future is dropped the object may or may not be
    /// visible, so callers run `commit` and the metadata publish together inside a
    /// task that is not tied to the request (see the hosted upload pipeline).
    ///
    /// # Errors
    /// Whatever `BlobWriter::commit` returns.
    pub async fn commit(mut self) -> Result<BlobCommit, BlobError> {
        match self.writer.take() {
            Some(writer) => writer.commit().await,
            None => Err(BlobError::WriterClosed),
        }
    }

    /// Discards everything written. Idempotent in effect; errors are logged, not returned,
    /// because the caller is already on a failure path.
    ///
    /// Cancel safe: if dropped half way, `BlobWriter::abort` is idempotent and the
    /// store cleans up on writer drop.
    pub async fn abort(mut self) {
        if let Some(writer) = self.writer.take()
            && let Err(error) = writer.abort().await
        {
            tracing::warn!(%error, key = self.key.as_str(), "blob writer abort failed");
        }
    }
}

impl Drop for BlobSink {
    fn drop(&mut self) {
        let Some(writer) = self.writer.take() else {
            return;
        };
        metrics::count(metrics::BLOB_WRITER_ABANDONED_TOTAL);
        // Drop cannot await. Hand the abort to the runtime if there is one; the
        // task is short and owns only the writer, so it needs no tracker.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            drop(handle.spawn(async move {
                if let Err(error) = writer.abort().await {
                    tracing::warn!(%error, "abort of an abandoned blob writer failed");
                }
            }));
        }
    }
}

/// Deletes a committed blob that must not be published (checksum mismatch, failed
/// metadata commit). A failure leaves an object in the store that no database row
/// points at, so it is counted and logged loudly: the row driven GC cannot find it.
///
/// Why there is no pending row that would make it findable (gap 28): core revision 2 rejected
/// it, because it would add a write to every upload and still not cover a lost commit
/// response of an object store. The leak is closed by the store sweep of `verify_blobs`
/// (`BlobStore::list_keys` against the `blobs` rows), which deletes orphan objects older than
/// twice the GC minimum age. The counter and the log line below are what an operator sees
/// first.
pub async fn delete_orphan(store: &dyn BlobStore, key: &BlobKey) {
    if let Err(error) = store.delete(key).await {
        metrics::count(metrics::BLOB_ORPHAN_LEAK_TOTAL);
        tracing::error!(%error, key = key.as_str(), "could not delete an unpublished blob; it is now an orphan object");
    }
}

/// Deletes the object of a commit that will not be published, through the store that took
/// the bytes. A group adapter refuses `delete`, so the member named in `placement` is the
/// one to ask; when even the lookup fails the object is leaked and counted like any other
/// failed cleanup.
pub(crate) async fn discard_committed(cx: &FormatContext, commit: &BlobCommit, key: &BlobKey) {
    match cx.store_of_commit(commit).await {
        Ok(store) => delete_orphan(store.as_ref(), key).await,
        Err(error) => {
            metrics::count(metrics::BLOB_ORPHAN_LEAK_TOTAL);
            tracing::error!(%error, key = key.as_str(), "no blob store to delete an unpublished blob from; it is now an orphan object");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_testkit::MemBlobStore;
    use std::time::Duration;

    async fn sink(store: &MemBlobStore, limit: u64) -> BlobSink {
        BlobSink::begin(store, BlobKey::new("k"), None, limit)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn writes_commit_and_report_size_and_digests() {
        let store = MemBlobStore::new();
        let mut sink = sink(&store, 100).await;
        sink.write(Bytes::from_static(b"hello ")).await.unwrap();
        sink.write(Bytes::from_static(b"world")).await.unwrap();
        assert_eq!(sink.written(), 11);
        assert_eq!(sink.key().as_str(), "k");
        let commit = sink.commit().await.unwrap();
        assert_eq!(commit.size_bytes, 11);
        assert_eq!(
            commit.checksums,
            rustryx_testkit::fixtures::checksums_of(b"hello world")
        );
        assert!(store.contains(&BlobKey::new("k")));
    }

    #[tokio::test]
    async fn must_not_write_the_chunk_that_crosses_the_limit() {
        let store = MemBlobStore::new();
        let mut sink = sink(&store, 8).await;
        sink.write(Bytes::from_static(b"12345")).await.unwrap();
        let error = sink.write(Bytes::from_static(b"6789")).await.unwrap_err();
        assert!(matches!(error, SinkError::LimitExceeded { limit: 8 }));
        assert_eq!(
            sink.written(),
            5,
            "nothing of the offending chunk was accepted"
        );
        sink.abort().await;
        assert_eq!(store.staging_count(), 0);
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn large_chunks_are_written_in_slices_of_at_most_64_kib() {
        let store = MemBlobStore::new();
        let mut sink = sink(&store, u64::MAX).await;
        sink.write(Bytes::from(vec![1u8; 3 * 1024 * 1024 + 5]))
            .await
            .unwrap();
        let commit = sink.commit().await.unwrap();
        assert_eq!(commit.size_bytes, 3 * 1024 * 1024 + 5);
    }

    #[tokio::test]
    async fn a_failing_store_surfaces_as_a_blob_error() {
        let store = MemBlobStore::new();
        let mut sink = sink(&store, 100).await;
        store.set_unavailable(true);
        let error = sink.write(Bytes::from_static(b"x")).await.unwrap_err();
        assert!(matches!(error, SinkError::Blob(BlobError::Unavailable(_))));
        sink.abort().await;
        store.set_unavailable(false);
        assert_eq!(store.staging_count(), 0);
    }

    #[tokio::test]
    async fn a_dropped_sink_aborts_its_writer_on_the_runtime() {
        let store = MemBlobStore::new();
        store.set_keep_abandoned_staging(true);
        let sink = sink(&store, 100).await;
        assert_eq!(store.staging_count(), 1);
        drop(sink);
        for _ in 0..100 {
            if store.staging_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the abandoned writer was never aborted");
    }

    #[tokio::test]
    async fn must_not_panic_when_an_orphan_cannot_be_deleted() {
        let store = MemBlobStore::new();
        store.set_unavailable(true);
        // Counted and logged, never propagated: the caller is already on a failure path.
        delete_orphan(&store, &BlobKey::new("gone")).await;
    }

    #[test]
    fn dropping_a_sink_without_a_runtime_does_not_panic() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = MemBlobStore::new();
        let sink = runtime.block_on(sink(&store, 10));
        drop(runtime);
        // No runtime any more: the store's own drop semantics are the only cleanup.
        drop(sink);
    }
}
