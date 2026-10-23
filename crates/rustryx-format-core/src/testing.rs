//! Test support for code that sits on top of this crate (format crates, the API
//! layer): a scriptable [`FakeUpstream`] and a synthetic body generator.
//!
//! It lives here, not in `rustryx-testkit`, because it implements this crate's
//! `UpstreamClient`. The testkit may not depend on format-core: a dev-dependency
//! cycle would give the test build two distinct copies of every shared type.
//! Nothing in this module is used on a production path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};

use crate::proxy::upstream::{UpstreamClient, UpstreamError, UpstreamRequest, UpstreamResponse};

/// One scripted answer of a [`FakeUpstream`] route.
#[derive(Debug, Clone)]
pub struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    chunks: Vec<Bytes>,
    chunk_delay: Duration,
    head_delay: Duration,
    fail_after_chunks: Option<usize>,
    error: Option<UpstreamError>,
    hang: bool,
}

impl Reply {
    /// A `200 OK` with the body as one chunk and a matching `Content-Length`
    /// (override it with [`Reply::header`] to simulate a lying upstream).
    #[must_use]
    pub fn ok(body: impl Into<Bytes>) -> Self {
        let body: Bytes = body.into();
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_LENGTH,
            HeaderValue::from(body.len() as u64),
        );
        Self {
            status: StatusCode::OK,
            headers,
            chunks: vec![body],
            chunk_delay: Duration::ZERO,
            head_delay: Duration::ZERO,
            fail_after_chunks: None,
            error: None,
            hang: false,
        }
    }

    /// An answer with the given status and an empty body.
    #[must_use]
    pub fn status(code: u16) -> Self {
        let mut reply = Self::ok(Bytes::new());
        reply.status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        reply.chunks.clear();
        reply.headers.clear();
        reply
    }

    /// A transport error instead of an answer.
    #[must_use]
    pub fn error(error: UpstreamError) -> Self {
        let mut reply = Self::status(500);
        reply.error = Some(error);
        reply
    }

    /// Never answers (the future stays pending), for timeout tests.
    #[must_use]
    pub fn hang() -> Self {
        let mut reply = Self::status(500);
        reply.hang = true;
        reply
    }

    /// A redirect to `location`.
    #[must_use]
    pub fn redirect(code: u16, location: &str) -> Self {
        Self::status(code).header("location", location)
    }

    /// Adds a response header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            self.headers.insert(name, value);
        }
        self
    }

    /// Removes the `Content-Length` header, like a chunked upstream that does not announce a size.
    #[must_use]
    pub fn without_content_length(mut self) -> Self {
        self.headers.remove(http::header::CONTENT_LENGTH);
        self
    }

    /// Splits the body into chunks of `size` bytes and waits `delay` before each one.
    #[must_use]
    pub fn chunked(mut self, size: usize, delay: Duration) -> Self {
        let whole: Vec<u8> = self.chunks.iter().flat_map(|c| c.iter().copied()).collect();
        self.chunks = whole
            .chunks(size.max(1))
            .map(Bytes::copy_from_slice)
            .collect();
        self.chunk_delay = delay;
        self
    }

    /// Waits before the response head is returned.
    #[must_use]
    pub fn delay_head(mut self, delay: Duration) -> Self {
        self.head_delay = delay;
        self
    }

    /// Breaks the body with a read error after `n` chunks.
    #[must_use]
    pub fn fail_after_chunks(mut self, n: usize) -> Self {
        self.fail_after_chunks = Some(n);
        self
    }
}

/// A request as the fake saw it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// Method.
    pub method: Method,
    /// Full URL text.
    pub url: String,
    /// Path component.
    pub path: String,
    /// Request headers.
    pub headers: HeaderMap,
}

#[derive(Default)]
struct State {
    routes: HashMap<String, (Vec<Reply>, usize)>,
    recorded: Vec<RecordedRequest>,
}

/// A scriptable [`UpstreamClient`]: routes by URL path, counts calls, can delay,
/// hang, break mid-body or fail. Unknown paths answer `404`.
#[derive(Default)]
pub struct FakeUpstream {
    state: Mutex<State>,
    calls: AtomicUsize,
}

impl FakeUpstream {
    /// Creates an empty fake.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Scripts the answers for a path. They are used in order; the last one repeats.
    pub fn route(&self, path: &str, replies: Vec<Reply>) {
        self.lock().routes.insert(path.to_owned(), (replies, 0));
    }

    /// Total number of requests received.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Number of requests for one path.
    #[must_use]
    pub fn calls_for(&self, path: &str) -> usize {
        self.lock()
            .recorded
            .iter()
            .filter(|r| r.path == path)
            .count()
    }

    /// All requests received, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.lock().recorded.clone()
    }

    fn next_reply(&self, request: &UpstreamRequest) -> Reply {
        let mut state = self.lock();
        state.recorded.push(RecordedRequest {
            method: request.method.clone(),
            url: request.url.to_string(),
            path: request.url.path().to_owned(),
            headers: request.headers.clone(),
        });
        match state.routes.get_mut(request.url.path()) {
            Some((replies, index)) if !replies.is_empty() => {
                let reply = replies[(*index).min(replies.len() - 1)].clone();
                *index += 1;
                reply
            }
            _ => Reply::status(404),
        }
    }
}

#[async_trait]
impl UpstreamClient for FakeUpstream {
    async fn send(&self, request: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = self.next_reply(&request);
        if reply.hang {
            futures::future::pending::<()>().await;
        }
        if !reply.head_delay.is_zero() {
            tokio::time::sleep(reply.head_delay).await;
        }
        if let Some(error) = reply.error {
            return Err(error);
        }
        let body = if request.method == Method::HEAD {
            stream::empty().boxed()
        } else {
            reply_body(reply.chunks, reply.chunk_delay, reply.fail_after_chunks)
        };
        Ok(UpstreamResponse {
            status: reply.status,
            headers: reply.headers,
            body,
        })
    }
}

fn reply_body(
    chunks: Vec<Bytes>,
    delay: Duration,
    fail_after: Option<usize>,
) -> BoxStream<'static, Result<Bytes, UpstreamError>> {
    stream::unfold(
        (chunks.into_iter(), 0usize),
        move |(mut it, sent)| async move {
            if fail_after == Some(sent) {
                let error = UpstreamError::Body("scripted failure".into());
                return Some((Err(error), (Vec::new().into_iter(), usize::MAX)));
            }
            if sent == usize::MAX {
                return None;
            }
            let chunk = it.next()?;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            Some((Ok(chunk), (it, sent + 1)))
        },
    )
    .boxed()
}

/// A deterministic body of `total` bytes in chunks of `chunk` bytes (the byte at
/// offset `i` is `i % 251`), generated lazily: memory use is one chunk however
/// large `total` is.
pub fn synthetic_stream(
    total: u64,
    chunk: usize,
) -> BoxStream<'static, Result<Bytes, std::io::Error>> {
    synthetic_range(0, total, chunk)
}

/// The bytes `start..end` of the synthetic body of [`synthetic_stream`], so a test
/// store can serve ranges of an object it never kept.
pub fn synthetic_range(
    start: u64,
    end: u64,
    chunk: usize,
) -> BoxStream<'static, Result<Bytes, std::io::Error>> {
    const PERIOD: usize = 251;
    // One buffer of whole periods, built once: chunks are then plain memcpy slices of it,
    // which keeps a multi-gigabyte test fast even in a debug build.
    let pattern: std::sync::Arc<Vec<u8>> = std::sync::Arc::new(
        (0..PERIOD * 1024)
            .map(|i| u8::try_from(i % PERIOD).unwrap_or(0))
            .collect(),
    );
    stream::unfold(start, move |offset| {
        let pattern = std::sync::Arc::clone(&pattern);
        async move {
            if offset >= end {
                return None;
            }
            let len = usize::try_from((end - offset).min(chunk as u64)).unwrap_or(chunk);
            let mut out = Vec::with_capacity(len);
            let mut position = usize::try_from(offset % PERIOD as u64).unwrap_or(0);
            while out.len() < len {
                let take = (len - out.len()).min(pattern.len() - position);
                out.extend_from_slice(&pattern[position..position + take]);
                position = (position + take) % PERIOD;
            }
            Some((Ok(Bytes::from(out)), offset + len as u64))
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn request(path: &str) -> UpstreamRequest {
        UpstreamRequest {
            method: Method::GET,
            url: Url::parse(&format!("https://up.example{path}")).unwrap(),
            headers: HeaderMap::new(),
        }
    }

    async fn collect(response: UpstreamResponse) -> (Vec<u8>, bool) {
        let mut body = response.body;
        let mut out = Vec::new();
        let mut failed = false;
        while let Some(item) = body.next().await {
            match item {
                Ok(chunk) => out.extend_from_slice(&chunk),
                Err(_) => failed = true,
            }
        }
        (out, failed)
    }

    #[tokio::test]
    async fn routes_replies_in_order_and_repeats_the_last() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::status(500), Reply::ok("fine")]);
        assert_eq!(fake.send(request("/a")).await.unwrap().status, 500);
        let second = fake.send(request("/a")).await.unwrap();
        assert_eq!(collect(second).await.0, b"fine");
        assert_eq!(fake.send(request("/a")).await.unwrap().status, 200);
        assert_eq!(fake.send(request("/unknown")).await.unwrap().status, 404);
        assert_eq!(fake.calls(), 4);
        assert_eq!(fake.calls_for("/a"), 3);
        assert_eq!(fake.requests().len(), 4);
    }

    #[tokio::test]
    async fn headers_chunks_and_errors() {
        let fake = FakeUpstream::new();
        fake.route(
            "/h",
            vec![
                Reply::ok("abcdef")
                    .header("x-test", "1")
                    .chunked(2, Duration::ZERO),
            ],
        );
        let response = fake.send(request("/h")).await.unwrap();
        assert_eq!(response.headers.get("x-test").unwrap(), "1");
        assert_eq!(collect(response).await, (b"abcdef".to_vec(), false));

        fake.route("/e", vec![Reply::error(UpstreamError::CircuitOpen)]);
        assert!(matches!(
            fake.send(request("/e")).await,
            Err(UpstreamError::CircuitOpen)
        ));

        fake.route(
            "/f",
            vec![
                Reply::ok("abcdef")
                    .chunked(2, Duration::ZERO)
                    .fail_after_chunks(1),
            ],
        );
        let (bytes, failed) = collect(fake.send(request("/f")).await.unwrap()).await;
        assert_eq!(bytes, b"ab");
        assert!(failed);

        fake.route("/r", vec![Reply::redirect(302, "/target")]);
        let redirect = fake.send(request("/r")).await.unwrap();
        assert_eq!(redirect.headers.get("location").unwrap(), "/target");
    }

    #[tokio::test(start_paused = true)]
    async fn head_requests_get_no_body_and_delays_use_the_tokio_clock() {
        let fake = FakeUpstream::new();
        fake.route(
            "/x",
            vec![Reply::ok("body").delay_head(Duration::from_secs(5))],
        );
        let mut head = request("/x");
        head.method = Method::HEAD;
        let started = tokio::time::Instant::now();
        let response = fake.send(head).await.unwrap();
        assert!(started.elapsed() >= Duration::from_secs(5));
        assert_eq!(collect(response).await.0, Vec::<u8>::new());
    }

    #[tokio::test(start_paused = true)]
    async fn hang_never_returns() {
        let fake = FakeUpstream::new();
        fake.route("/h", vec![Reply::hang()]);
        let result =
            tokio::time::timeout(Duration::from_secs(3600), fake.send(request("/h"))).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn synthetic_stream_is_deterministic_and_bounded_by_chunk() {
        let mut s = synthetic_stream(1000, 300);
        let mut total = 0usize;
        let mut first = Vec::new();
        while let Some(chunk) = s.next().await {
            let chunk = chunk.unwrap();
            assert!(chunk.len() <= 300);
            if first.is_empty() {
                first = chunk.to_vec();
            }
            total += chunk.len();
        }
        assert_eq!(total, 1000);
        assert_eq!(first[0], 0);
        assert_eq!(first[251], 0);
        assert_eq!(first[1], 1);
    }
}
