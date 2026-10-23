//! Stream adapters and bounded reads.
//!
//! Everything here is about one rule: a body is a stream, and the only thing ever
//! held in memory is one chunk, or a metadata document up to an explicit limit.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum_core::body::Body;
use bytes::{Bytes, BytesMut};
use futures::stream::BoxStream;
use futures::{Stream, StreamExt, TryStreamExt};
use rustryx_core::error::BlobError;
use rustryx_core::ports::BlobRead;
use tokio::time::{Instant, Sleep};

/// The checksum slice size of the hard rules: digests are fed in pieces of at most
/// this many bytes so one `write` call never hashes a huge buffer in one go.
pub const HASH_SLICE: usize = 64 * 1024;

/// Error of a [`SizeLimited`] stream.
#[derive(Debug, thiserror::Error)]
pub enum LimitError<E> {
    /// More than `limit` bytes arrived.
    #[error("stream exceeded {limit} bytes")]
    Exceeded {
        /// The limit that was crossed.
        limit: u64,
    },
    /// The wrapped stream failed.
    #[error(transparent)]
    Inner(E),
}

/// Stream adapter that fails once more than `limit` bytes have passed through.
///
/// It counts actual bytes, never trusting `Content-Length`: a lying client or a
/// chunked body is cut at the limit all the same. After the error the stream ends.
pub struct SizeLimited<S> {
    inner: S,
    seen: u64,
    limit: u64,
    done: bool,
}

impl<S> SizeLimited<S> {
    /// Wraps `inner` with a byte limit.
    #[must_use]
    pub fn new(inner: S, limit: u64) -> Self {
        Self {
            inner,
            seen: 0,
            limit,
            done: false,
        }
    }

    /// Bytes that have passed through so far.
    #[must_use]
    pub fn seen(&self) -> u64 {
        self.seen
    }
}

impl<S, E> Stream for SizeLimited<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, LimitError<E>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => {
                self.done = true;
                Poll::Ready(Some(Err(LimitError::Inner(e))))
            }
            Poll::Ready(Some(Ok(chunk))) => {
                self.seen = self.seen.saturating_add(chunk.len() as u64);
                if self.seen > self.limit {
                    self.done = true;
                    Poll::Ready(Some(Err(LimitError::Exceeded { limit: self.limit })))
                } else {
                    Poll::Ready(Some(Ok(chunk)))
                }
            }
        }
    }
}

/// Error of an [`IdleTimeout`] stream.
#[derive(Debug, thiserror::Error)]
pub enum IdleError<E> {
    /// No chunk arrived within the idle window.
    #[error("no data for {0:?}")]
    Idle(Duration),
    /// The absolute deadline passed.
    #[error("deadline exceeded")]
    Deadline,
    /// The wrapped stream failed.
    #[error(transparent)]
    Inner(E),
}

/// Stream adapter with an idle timeout and an optional absolute deadline.
///
/// The idle timer restarts after every chunk, so a slow but steady transfer never
/// times out and a stalled one does after `idle`, with no total ceiling (README 8.2:
/// the data stream is bounded by idleness only). The deadline exists for metadata
/// fetches, which have a total budget. After an error the stream ends.
pub struct IdleTimeout<S> {
    inner: S,
    idle: Duration,
    timer: Pin<Box<Sleep>>,
    deadline: Option<Pin<Box<Sleep>>>,
    done: bool,
}

impl<S> IdleTimeout<S> {
    /// Wraps `inner`. `deadline` is an absolute instant after which the stream fails.
    #[must_use]
    pub fn new(inner: S, idle: Duration, deadline: Option<Instant>) -> Self {
        Self {
            inner,
            idle,
            timer: Box::pin(tokio::time::sleep(idle)),
            deadline: deadline.map(|at| Box::pin(tokio::time::sleep_until(at))),
            done: false,
        }
    }
}

impl<S, E> Stream for IdleTimeout<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, IdleError<E>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                let next = Instant::now() + self.idle;
                self.timer.as_mut().reset(next);
                return Poll::Ready(Some(Ok(chunk)));
            }
            Poll::Ready(Some(Err(e))) => {
                self.done = true;
                return Poll::Ready(Some(Err(IdleError::Inner(e))));
            }
            Poll::Ready(None) => {
                self.done = true;
                return Poll::Ready(None);
            }
            Poll::Pending => {}
        }
        if let Some(deadline) = self.deadline.as_mut()
            && deadline.as_mut().poll(cx).is_ready()
        {
            self.done = true;
            return Poll::Ready(Some(Err(IdleError::Deadline)));
        }
        if self.timer.as_mut().poll(cx).is_ready() {
            self.done = true;
            return Poll::Ready(Some(Err(IdleError::Idle(self.idle))));
        }
        Poll::Pending
    }
}

/// Splits a chunk into slices of at most [`HASH_SLICE`] bytes without copying.
pub fn hash_slices(chunk: Bytes) -> impl Iterator<Item = Bytes> {
    let len = chunk.len();
    (0..len.div_ceil(HASH_SLICE)).map(move |i| {
        let start = i * HASH_SLICE;
        chunk.slice(start..len.min(start + HASH_SLICE))
    })
}

/// Why [`read_bounded`] gave up.
#[derive(Debug, thiserror::Error)]
pub enum BoundedReadError {
    /// More than the limit arrived.
    #[error("body exceeds {limit} bytes")]
    TooLarge {
        /// The limit that was crossed.
        limit: u64,
    },
    /// The body could not be read to the end.
    #[error("body read failed: {0}")]
    Read(String),
}

/// Reads a body into memory up to `limit` bytes.
///
/// This is the only sanctioned way to buffer a body: for metadata documents with a
/// limit chosen by the caller. Artifacts must never come through here. The limit is
/// enforced on actual bytes, so memory use is bounded by `limit` plus one chunk.
///
/// Cancel safe: dropping the future just drops the partial buffer.
///
/// # Errors
/// [`BoundedReadError::TooLarge`] when the limit is crossed, [`BoundedReadError::Read`]
/// when the stream fails.
pub async fn read_bounded(body: Body, limit: u64) -> Result<Bytes, BoundedReadError> {
    collect_bounded(body.into_data_stream(), limit).await
}

/// Like [`read_bounded`] for any stream of chunks with a displayable error.
///
/// # Errors
/// See [`read_bounded`].
pub async fn collect_bounded<S, E>(mut stream: S, limit: u64) -> Result<Bytes, BoundedReadError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buffer = BytesMut::new();
    while let Some(next) = stream.next().await {
        let chunk = next.map_err(|e| BoundedReadError::Read(e.to_string()))?;
        if (buffer.len() as u64).saturating_add(chunk.len() as u64) > limit {
            return Err(BoundedReadError::TooLarge { limit });
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer.freeze())
}

/// Turns an open blob read into a response body. The stream is passed through
/// as is, so backpressure from the client reaches the blob store.
pub fn body_from_blob(read: BlobRead) -> Body {
    Body::from_stream(read.stream.map_err(|error| blob_error_to_io(&error)))
}

/// Turns any chunk stream into a response body.
pub fn body_from_chunks(stream: BoxStream<'static, io::Result<Bytes>>) -> Body {
    Body::from_stream(stream)
}

fn blob_error_to_io(error: &BlobError) -> io::Error {
    // The text only reaches the connection-abort path, never a response body.
    io::Error::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    fn chunks(sizes: &[usize]) -> impl Stream<Item = Result<Bytes, io::Error>> + Unpin + use<> {
        stream::iter(
            sizes
                .iter()
                .map(|&n| Ok(Bytes::from(vec![7u8; n])))
                .collect::<Vec<_>>(),
        )
    }

    #[tokio::test]
    async fn size_limited_passes_up_to_limit() {
        let mut s = SizeLimited::new(chunks(&[4, 4]), 8);
        assert_eq!(s.next().await.unwrap().unwrap().len(), 4);
        assert_eq!(s.next().await.unwrap().unwrap().len(), 4);
        assert!(s.next().await.is_none());
        assert_eq!(s.seen(), 8);
    }

    #[tokio::test]
    async fn must_not_let_more_than_limit_through() {
        let mut s = SizeLimited::new(chunks(&[4, 4, 4]), 9);
        assert!(s.next().await.unwrap().is_ok());
        assert!(s.next().await.unwrap().is_ok());
        let err = s.next().await.unwrap().unwrap_err();
        assert!(matches!(err, LimitError::Exceeded { limit: 9 }));
        assert!(s.next().await.is_none(), "stream must end after the error");
    }

    #[tokio::test]
    async fn size_limited_propagates_inner_error_once() {
        let inner = stream::iter(vec![Err::<Bytes, _>(io::Error::other("boom"))]);
        let mut s = SizeLimited::new(inner, 10);
        assert!(matches!(s.next().await.unwrap(), Err(LimitError::Inner(_))));
        assert!(s.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn idle_timeout_fires_only_when_stalled() {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, io::Error>>(1);
        let rx = Box::pin(stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }));
        let mut s = IdleTimeout::new(rx, Duration::from_secs(60), None);
        tx.send(Ok(Bytes::from_static(b"a"))).await.unwrap();
        assert!(s.next().await.unwrap().is_ok());
        // A steady slow trickle (59 s apart) must keep the stream alive forever.
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_secs(59)).await;
            tx.send(Ok(Bytes::from_static(b"b"))).await.unwrap();
            assert!(s.next().await.unwrap().is_ok());
        }
        let err = s.next().await.unwrap().unwrap_err();
        assert!(matches!(err, IdleError::Idle(d) if d == Duration::from_secs(60)));
        assert!(s.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_cuts_off_a_busy_stream() {
        let ticking = Box::pin(stream::unfold(0u8, |n| async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Some((Ok::<_, io::Error>(Bytes::from_static(b"x")), n + 1))
        }));
        let mut s = IdleTimeout::new(
            ticking,
            Duration::from_secs(60),
            Some(Instant::now() + Duration::from_secs(25)),
        );
        assert!(s.next().await.unwrap().is_ok());
        assert!(s.next().await.unwrap().is_ok());
        let err = s.next().await.unwrap().unwrap_err();
        assert!(matches!(err, IdleError::Deadline));
    }

    #[tokio::test]
    async fn idle_timeout_passes_end_and_errors() {
        let mut s = IdleTimeout::new(chunks(&[1]), Duration::from_secs(1), None);
        assert!(s.next().await.unwrap().is_ok());
        assert!(s.next().await.is_none());
        let failing = stream::iter(vec![Err::<Bytes, _>(io::Error::other("x"))]);
        let mut s = IdleTimeout::new(failing, Duration::from_secs(1), None);
        assert!(matches!(s.next().await.unwrap(), Err(IdleError::Inner(_))));
        assert!(s.next().await.is_none());
    }

    #[test]
    fn hash_slices_cover_chunk_without_copy() {
        let chunk = Bytes::from(vec![1u8; HASH_SLICE * 2 + 10]);
        let parts: Vec<_> = hash_slices(chunk.clone()).collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.len() <= HASH_SLICE));
        assert_eq!(parts.iter().map(Bytes::len).sum::<usize>(), chunk.len());
        assert_eq!(hash_slices(Bytes::new()).count(), 0);
    }

    #[tokio::test]
    async fn read_bounded_collects_within_limit() {
        let body = Body::from("hello world");
        let bytes = read_bounded(body, 11).await.unwrap();
        assert_eq!(&bytes[..], b"hello world");
    }

    #[tokio::test]
    async fn must_not_buffer_more_than_the_bounded_read_limit() {
        let err = read_bounded(Body::from("hello world"), 10)
            .await
            .unwrap_err();
        assert!(matches!(err, BoundedReadError::TooLarge { limit: 10 }));
        let failing = Body::from_stream(stream::iter(vec![
            Ok::<_, io::Error>(Bytes::from_static(b"ab")),
            Err(io::Error::other("reset")),
        ]));
        let err = read_bounded(failing, 100).await.unwrap_err();
        assert!(matches!(err, BoundedReadError::Read(_)));
    }

    #[tokio::test]
    async fn blob_body_streams_chunks_and_maps_errors() {
        let read = BlobRead {
            total_size: 6,
            stream: stream::iter(vec![
                Ok(Bytes::from_static(b"abc")),
                Ok(Bytes::from_static(b"def")),
            ])
            .boxed(),
        };
        let bytes = read_bounded(body_from_blob(read), 100).await.unwrap();
        assert_eq!(&bytes[..], b"abcdef");
        let failing = BlobRead {
            total_size: 1,
            stream: stream::iter(vec![Err(BlobError::NotFound)]).boxed(),
        };
        assert!(read_bounded(body_from_blob(failing), 100).await.is_err());
    }

    #[tokio::test]
    async fn chunk_body_passes_through() {
        let body = body_from_chunks(
            stream::iter(vec![Ok::<_, io::Error>(Bytes::from_static(b"xy"))]).boxed(),
        );
        assert_eq!(&read_bounded(body, 10).await.unwrap()[..], b"xy");
    }
}
