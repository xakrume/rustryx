//! The S3 half of a write: bounded buffer, lazy multipart upload, guaranteed abort.
//!
//! Memory model: the sink owns one buffer of at most `part_size` bytes. `write`
//! does not return while a full part is being uploaded, so no second part is ever
//! buffered. Memory per upload is therefore one part, which is what lets the
//! upload semaphore double as a memory budget.
//!
//! Visibility model: nothing exists in the bucket until the single `PutObject` (for
//! objects up to one part) or `CompleteMultipartUpload` at commit. The multipart
//! upload itself is created lazily when the first part fills up, so small
//! artifacts, which are the common case, cost one request instead of three.

use std::sync::Arc;

use bytes::BytesMut;
use object_store::multipart::PartId;
use object_store::path::Path as ObjectPath;
use object_store::{MultipartId, PutOptions, PutPayload};
use rustryx_core::BlobError;
use tokio::sync::OwnedSemaphorePermit;

use super::{Backend, store_error};
use crate::config::MAX_PARTS;
use crate::error::{backend, with_deadline};
use crate::hashing::BlobSink;

pub(crate) struct S3Sink {
    backend: Arc<Backend>,
    path: ObjectPath,
    part_size: usize,
    /// Capacity to allocate at the start of a part when the caller announced at least
    /// that much data: one allocation instead of ~8 doubling reallocations per part.
    /// Zero when the size is unknown, so a small artifact never reserves a full part.
    first_alloc: usize,
    /// Invariant: `buf.len() <= part_size` and `buf.capacity() <= part_size` after
    /// every call. `reserve_bounded` is what holds the capacity half of it.
    buf: BytesMut,
    upload: Option<MultipartId>,
    parts: Vec<PartId>,
    /// Upload slot. Released when the sink is dropped, which happens at commit,
    /// abort and drop alike; it is never leaked by an early return.
    _permit: OwnedSemaphorePermit,
    /// True once `finish` or `discard` ran. `Drop` aborts the remote upload only
    /// while this is false.
    settled: bool,
}

impl S3Sink {
    pub(crate) fn new(
        backend: Arc<Backend>,
        path: ObjectPath,
        part_size: usize,
        size_hint: Option<u64>,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        // Pre-size only when the caller promised at least one full part; otherwise
        // grow on demand so a 2 KiB artifact does not reserve 16 MiB.
        let initial = size_hint
            .and_then(|hint| usize::try_from(hint).ok())
            .map_or(0, |hint| hint.min(part_size));
        Self {
            backend,
            path,
            part_size,
            first_alloc: initial,
            buf: BytesMut::with_capacity(initial),
            upload: None,
            parts: Vec::new(),
            _permit: permit,
            settled: false,
        }
    }

    /// Makes room for `additional` more bytes without ever exceeding `part_size`.
    /// `BytesMut::reserve` may double the capacity past the part size, so growth is
    /// done by hand: allocate the target once and copy.
    ///
    /// The next part's buffer is allocated here, lazily, and never right after a
    /// flush: allocating it before the previous part finished uploading would hold
    /// two parts at once and break the one-part memory bound.
    fn reserve_bounded(&mut self, additional: usize) {
        let needed = self.buf.len() + additional;
        if needed <= self.buf.capacity() {
            return;
        }
        let start = if self.buf.capacity() == 0 {
            self.first_alloc
        } else {
            0
        };
        let target = needed
            .max(self.buf.capacity().saturating_mul(2))
            .max(start)
            .min(self.part_size);
        let mut grown = BytesMut::with_capacity(target);
        grown.extend_from_slice(&self.buf);
        self.buf = grown;
    }

    async fn upload_id(&mut self) -> Result<MultipartId, BlobError> {
        if let Some(id) = &self.upload {
            return Ok(id.clone());
        }
        let multipart = Arc::clone(&self.backend.multipart);
        let path = self.path.clone();
        let id = with_deadline(
            "s3 create multipart",
            self.backend.settings.metadata_timeout,
            async {
                multipart
                    .create_multipart(&path)
                    .await
                    .map_err(|e| store_error("create multipart upload", e))
            },
        )
        .await?;
        self.upload = Some(id.clone());
        Ok(id)
    }

    /// Uploads the buffered bytes as the next part and empties the buffer.
    async fn flush_part(&mut self) -> Result<(), BlobError> {
        if self.parts.len() >= MAX_PARTS {
            return Err(backend(
                "upload part",
                "object needs more than 10000 parts; raise `part_size_mib` or pass a size hint",
            ));
        }
        let id = self.upload_id().await?;
        // `split` hands the whole allocation to the payload and leaves this buffer
        // empty with no capacity, so the part is freed as soon as its upload ends.
        let payload = PutPayload::from(self.buf.split().freeze());
        let index = self.parts.len();
        let multipart = Arc::clone(&self.backend.multipart);
        let path = self.path.clone();
        let part = with_deadline(
            "s3 upload part",
            self.backend.settings.part_timeout,
            async {
                multipart
                    .put_part(&path, &id, index, payload)
                    .await
                    .map_err(|e| store_error("upload part", e))
            },
        )
        .await?;
        self.parts.push(part);
        Ok(())
    }

    async fn complete(&mut self) -> Result<(), BlobError> {
        let backend_handle = Arc::clone(&self.backend);
        let path = self.path.clone();
        let deadline = backend_handle.settings.part_timeout;
        if self.upload.is_none() {
            let payload = PutPayload::from(self.buf.split().freeze());
            return with_deadline("s3 put object", deadline, async {
                backend_handle
                    .store
                    .put_opts(&path, payload, PutOptions::default())
                    .await
                    .map(|_| ())
                    .map_err(|e| store_error("put object", e))
            })
            .await;
        }
        if !self.buf.is_empty() {
            self.flush_part().await?;
        }
        let id = self.upload_id().await?;
        let parts = std::mem::take(&mut self.parts);
        with_deadline("s3 complete multipart", deadline, async {
            backend_handle
                .multipart
                .complete_multipart(&path, &id, parts)
                .await
                .map(|_| ())
                .map_err(|e| store_error("complete multipart upload", e))
        })
        .await?;
        // Completed: there is nothing left that an abort could clean up.
        self.upload = None;
        Ok(())
    }

    async fn abort_remote(&mut self) -> Result<(), BlobError> {
        let Some(id) = self.upload.take() else {
            return Ok(());
        };
        let multipart = Arc::clone(&self.backend.multipart);
        let path = self.path.clone();
        with_deadline(
            "s3 abort multipart",
            self.backend.settings.metadata_timeout,
            async {
                multipart
                    .abort_multipart(&path, &id)
                    .await
                    .map_err(|e| store_error("abort multipart upload", e))
            },
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn buffer_capacity(&self) -> usize {
        self.buf.capacity()
    }
}

impl BlobSink for S3Sink {
    async fn write(&mut self, data: &[u8]) -> Result<(), BlobError> {
        let mut rest = data;
        while !rest.is_empty() {
            // A full buffer is flushed only when more data arrives. An object of
            // exactly one part (or the full last part) then still takes the cheap
            // single-request path or is flushed by `complete`.
            if self.buf.len() == self.part_size {
                self.flush_part().await?;
            }
            let take = (self.part_size - self.buf.len()).min(rest.len());
            let (now, later) = rest.split_at(take);
            self.reserve_bounded(take);
            self.buf.extend_from_slice(now);
            rest = later;
        }
        Ok(())
    }

    async fn finish(mut self) -> Result<(), BlobError> {
        let outcome = self.complete().await;
        if let Err(err) = &outcome {
            // The commit failed, possibly after the server accepted some parts.
            // Abort so the parts do not sit in the bucket until the lifecycle rule
            // reaps them. The abort result is secondary to the commit error.
            if let Err(abort_err) = self.abort_remote().await {
                tracing::warn!(error = %abort_err, commit_error = %err, "abort after failed commit also failed");
            }
        }
        self.settled = true;
        outcome
    }

    async fn discard(mut self) -> Result<(), BlobError> {
        let outcome = self.abort_remote().await;
        self.settled = true;
        outcome
    }
}

impl Drop for S3Sink {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let Some(id) = self.upload.take() else {
            // No multipart upload was created: nothing exists remotely.
            return;
        };
        spawn_abort(&self.backend, self.path.clone(), id);
    }
}

/// Aborts a multipart upload from `Drop`, which cannot await. The task lives on the
/// store's tracker so shutdown can wait for it. If the abort cannot run (no runtime,
/// S3 down) the bucket lifecycle rule for incomplete uploads is the backstop.
fn spawn_abort(backend: &Arc<Backend>, path: ObjectPath, id: MultipartId) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            "no runtime to abort an abandoned multipart upload; the bucket lifecycle rule will reap it"
        );
        return;
    };
    let multipart = Arc::clone(&backend.multipart);
    let deadline = backend.settings.metadata_timeout;
    backend.cleanup.spawn_on(
        async move {
            match tokio::time::timeout(deadline, multipart.abort_multipart(&path, &id)).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "failed to abort an abandoned multipart upload");
                }
                Err(_elapsed) => {
                    tracing::warn!("timed out aborting an abandoned multipart upload");
                }
            }
        },
        &handle,
    );
}
