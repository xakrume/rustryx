//! The attachment of a publish as a byte stream.
//!
//! [`AttachmentStream`] turns the rest of a publish body into the decoded tarball: the hosted
//! upload pipeline pulls from it exactly as it would pull from a request body, so the payload goes
//! from the socket to the blob writer in pieces of one network read, and the only things kept are
//! the bounded metadata buffer of the [`Scanner`] and a SHA-512 state.
//!
//! The stream ends only when the whole JSON document has been read and is valid. That is on
//! purpose: the pipeline publishes when its input ends, so a document that is cut off, has
//! garbage after the payload, or whose payload does not match the SHA-512 the publisher declared
//! turns into an error item instead of an end, the pipeline aborts the staged blob, and nothing
//! is published. The reason for a failure travels out of band in [`Outcome`], because the
//! pipeline reports every stream error as the same generic bad request.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use axum_core::body::BodyDataStream;
use bytes::Bytes;
use futures::Stream;
use rustryx_format_core::stream::hash_slices;
use sha2::{Digest, Sha512};

use crate::scan::{ScanError, Scanner};

/// Why the stream failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFailure {
    /// The scanner refused the document.
    Scan(ScanError),
    /// The SHA-512 of the decoded payload is not the one the publisher declared.
    Integrity,
    /// The client body could not be read to the end.
    Read,
}

/// What a finished stream leaves behind.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Set when the stream ended with an error.
    pub failure: Option<StreamFailure>,
    /// Set when the stream ended normally.
    pub completed: Option<Completed>,
}

/// A fully read and verified publish body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completed {
    /// The complete metadata document, payload elided.
    pub meta: Vec<u8>,
    /// SHA-512 of the decoded payload.
    pub sha512: [u8; 64],
    /// Number of decoded payload bytes.
    pub size: u64,
}

/// Shared slot through which the stream reports to the code that started the upload.
#[derive(Debug, Clone, Default)]
pub struct OutcomeSlot(Arc<Mutex<Outcome>>);

impl OutcomeSlot {
    /// A fresh, empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Outcome> {
        // A poisoned lock only means a panic elsewhere; the data is a plain value.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the failure, if any.
    #[must_use]
    pub fn take_failure(&self) -> Option<StreamFailure> {
        self.lock().failure.take()
    }

    /// Takes the completion record, if the stream finished normally.
    #[must_use]
    pub fn take_completed(&self) -> Option<Completed> {
        self.lock().completed.take()
    }
}

/// The decoded attachment of a publish, as a stream of byte chunks.
pub struct AttachmentStream {
    source: BodyDataStream,
    scanner: Scanner,
    hasher: Sha512,
    expected_sha512: Option<[u8; 64]>,
    pending: Bytes,
    size: u64,
    ended: bool,
    slot: OutcomeSlot,
}

impl AttachmentStream {
    /// Continues a scan whose head has been read: `scanner` has seen the start of the payload and
    /// may hold decoded bytes already; `source` is the rest of the body.
    #[must_use]
    pub fn new(
        source: BodyDataStream,
        mut scanner: Scanner,
        expected_sha512: Option<[u8; 64]>,
        slot: OutcomeSlot,
    ) -> Self {
        let pending = Bytes::from(scanner.take_decoded());
        Self {
            source,
            scanner,
            hasher: Sha512::new(),
            expected_sha512,
            pending,
            size: 0,
            ended: false,
            slot,
        }
    }

    fn fail(&mut self, failure: StreamFailure) -> Poll<Option<io::Result<Bytes>>> {
        self.ended = true;
        self.slot.lock().failure = Some(failure);
        Poll::Ready(Some(Err(io::Error::other(
            "the publish document was refused",
        ))))
    }

    fn complete(&mut self) -> Poll<Option<io::Result<Bytes>>> {
        if let Err(error) = self.scanner.finish() {
            return self.fail(StreamFailure::Scan(error));
        }
        let digest: [u8; 64] = std::mem::take(&mut self.hasher).finalize().into();
        if self
            .expected_sha512
            .is_some_and(|expected| expected != digest)
        {
            return self.fail(StreamFailure::Integrity);
        }
        self.ended = true;
        self.slot.lock().completed = Some(Completed {
            meta: self.scanner.meta().to_vec(),
            sha512: digest,
            size: self.size,
        });
        Poll::Ready(None)
    }
}

impl Stream for AttachmentStream {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        loop {
            if this.ended {
                return Poll::Ready(None);
            }
            if !this.pending.is_empty() {
                let chunk = std::mem::take(&mut this.pending);
                // Hashed in slices of at most 64 KiB in this task, as the hard rules require.
                for slice in hash_slices(chunk.clone()) {
                    this.hasher.update(&slice);
                }
                this.size = this.size.saturating_add(chunk.len() as u64);
                return Poll::Ready(Some(Ok(chunk)));
            }
            match Pin::new(&mut this.source).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(bytes))) => {
                    if let Err(error) = this.scanner.feed(&bytes) {
                        return this.fail(StreamFailure::Scan(error));
                    }
                    this.pending = Bytes::from(this.scanner.take_decoded());
                }
                Poll::Ready(Some(Err(_))) => return this.fail(StreamFailure::Read),
                Poll::Ready(None) => return this.complete(),
            }
        }
    }
}

#[cfg(test)]
mod tests;
