//! The blocking pool of the pub format: a permit before the spawn, and a bridge that feeds a
//! synchronous reader from an asynchronous stream.
//!
//! Reading an archive is CPU work (inflate, tar, YAML) and belongs on `spawn_blocking`, behind a
//! semaphore whose permit is taken **before** the task is spawned (CONTRIBUTING rule 2): when no
//! permit is free within the deadline the client gets `503` and `Retry-After`, and no queue of
//! pending work builds up. Format-core's own blocking semaphore is private to the engine, so the
//! pub handler keeps one of its own; the two do not share a budget, which is a known gap
//! (documented in `docs/format-pub.md`).

use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes};
use futures::{Stream, StreamExt};
use rustryx_format_core::FormatError;
use tokio::sync::mpsc::{self, Receiver};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::metrics;

/// Chunks that may wait between the async reader and the blocking consumer. Two is enough to
/// keep the consumer fed and small enough that the archive is never held in memory: this is the
/// whole backpressure window.
const BRIDGE_CHUNKS: usize = 2;

/// A semaphore for blocking work, with a deadline.
#[derive(Debug, Clone)]
pub struct BlockingPool {
    semaphore: Arc<Semaphore>,
    wait: Duration,
}

impl BlockingPool {
    /// A pool of `permits` (at least one) that makes a caller wait at most `wait`.
    #[must_use]
    pub fn new(permits: usize, wait: Duration) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(permits.max(1))),
            wait,
        }
    }

    /// Takes a permit.
    ///
    /// # Errors
    /// [`FormatError::Unavailable`] (503 with `Retry-After`) when none is free in time.
    pub async fn acquire(&self) -> Result<OwnedSemaphorePermit, FormatError> {
        let waiting = tokio::time::timeout(self.wait, Arc::clone(&self.semaphore).acquire_owned());
        let Ok(Ok(permit)) = waiting.await else {
            metrics::blocking_refused();
            return Err(FormatError::Unavailable {
                reason: "the server is busy reading package archives, retry",
                retry_after: Some(Duration::from_secs(1)),
            });
        };
        Ok(permit)
    }

    /// Runs `work` on the blocking pool under a permit.
    ///
    /// # Errors
    /// [`FormatError::Unavailable`] without a permit, [`FormatError::Internal`] if the task
    /// panicked (a bug in a parser, which the fuzz targets exist to find).
    pub async fn run<T, F>(&self, work: F) -> Result<T, FormatError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let permit = self.acquire().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| FormatError::internal(format!("blocking task failed: {error}")))
    }

    /// Runs `work` on the blocking pool over a reader that is fed from `stream`.
    ///
    /// The stream is pumped on the calling task while `work` reads on a blocking thread, through
    /// a channel of [`BRIDGE_CHUNKS`] chunks, so a slow reader slows the source and nothing is
    /// buffered beyond the window. If the reader stops early (it found a reason to refuse), the
    /// pump notices at its next send and ends. If the caller is cancelled, the sender is dropped
    /// with the pump and the reader sees the end of the stream, so the blocking task finishes
    /// instead of waiting for bytes that never come.
    ///
    /// A failure of the stream (the blob store) is returned as `Err` from the stream's own error
    /// mapping and takes precedence over what the reader made of the truncated input: a store
    /// fault must not be reported as a bad archive.
    ///
    /// # Errors
    /// As [`BlockingPool::run`], or the error of the stream.
    pub async fn run_with_reader<T, F, S, E>(
        &self,
        stream: S,
        map_stream_error: impl Fn(E) -> FormatError,
        work: F,
    ) -> Result<T, FormatError>
    where
        T: Send + 'static,
        F: FnOnce(ChannelReader) -> T + Send + 'static,
        S: Stream<Item = Result<Bytes, E>> + Unpin,
    {
        let permit = self.acquire().await?;
        let (sender, receiver) = mpsc::channel::<io::Result<Bytes>>(BRIDGE_CHUNKS);
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work(ChannelReader::new(receiver))
        });
        let mut stream = stream;
        let mut stream_error: Option<FormatError> = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    if sender.send(Ok(chunk)).await.is_err() {
                        // The reader is done (or gone): nothing more is wanted.
                        break;
                    }
                }
                Err(error) => {
                    let mapped = map_stream_error(error);
                    // Tell the reader the input broke, then stop. The reader's error is
                    // discarded below in favour of the stream's.
                    let _ = sender.send(Err(io::Error::other("source failed"))).await;
                    stream_error = Some(mapped);
                    break;
                }
            }
        }
        drop(sender);
        let outcome = task
            .await
            .map_err(|error| FormatError::internal(format!("blocking task failed: {error}")))?;
        match stream_error {
            Some(error) => Err(error),
            None => Ok(outcome),
        }
    }
}

/// A [`Read`] over a channel of byte chunks, for use on a blocking thread.
#[derive(Debug)]
pub struct ChannelReader {
    receiver: Receiver<io::Result<Bytes>>,
    current: Bytes,
}

impl ChannelReader {
    fn new(receiver: Receiver<io::Result<Bytes>>) -> Self {
        Self {
            receiver,
            current: Bytes::new(),
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.current.is_empty() {
            // `blocking_recv` is the documented way to wait for a channel on a blocking thread;
            // this reader only ever runs inside `spawn_blocking`.
            match self.receiver.blocking_recv() {
                Some(Ok(chunk)) => self.current = chunk,
                Some(Err(error)) => return Err(error),
                None => return Ok(0),
            }
        }
        let n = self.current.len().min(buf.len());
        let Some(target) = buf.get_mut(..n) else {
            return Ok(0);
        };
        self.current.copy_to_slice(target);
        Ok(n)
    }
}

#[cfg(test)]
mod tests;
