//! Resilient upstream calls: the layer between the proxy pipeline and an
//! [`UpstreamClient`].
//!
//! Per call it applies, in this order: SSRF check of the URL, the circuit breaker
//! of the upstream host, the per-repository concurrency cap, the request with
//! redirect following (every hop re-validated, credentials never leave the
//! original origin), and retries with jittered backoff for `GET` and `HEAD`.
//!
//! Permit discipline (hard rule, SRE): the concurrency permit is taken right
//! before the request and is released as soon as the response body is finished or
//! dropped. It is *not* held while sleeping between retries, and nothing here holds
//! a lock across an await.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use dashmap::DashMap;
use futures::StreamExt;
use futures::stream::BoxStream;
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use rustryx_core::ids::RepositoryId;
use rustryx_core::model::UpstreamAuth;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use url::Url;

use super::breaker::{BreakerConfig, CircuitBreaker};
use super::ssrf::UpstreamUrlPolicy;
use super::upstream::{TimeoutPhase, UpstreamClient, UpstreamError, UpstreamRequest};
use crate::metrics;
use crate::stream::{IdleError, IdleTimeout};

/// Retry policy for idempotent upstream requests (SRE-24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first one. README 8.2: at most three.
    pub max_attempts: u32,
    /// Backoff base: the ceiling of the first retry delay.
    pub base_delay: Duration,
    /// Upper bound for any single delay, including a server-sent `Retry-After`.
    pub max_delay: Duration,
    /// Full jitter: sleep a random time in `0..=ceiling` instead of the ceiling.
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(5),
            jitter: true,
        }
    }
}

impl RetryPolicy {
    /// Delay before the next attempt, given the number (1-based) of the attempt
    /// that just failed and an optional `Retry-After` from the upstream.
    #[must_use]
    pub fn delay_before_retry(
        &self,
        failed_attempt: u32,
        retry_after: Option<Duration>,
    ) -> Duration {
        let exponent = failed_attempt.saturating_sub(1).min(16);
        let ceiling = self
            .base_delay
            .saturating_mul(1u32 << exponent)
            .min(self.max_delay);
        let backoff = if self.jitter {
            let max_ms = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
            Duration::from_millis(rand::random_range(0..=max_ms))
        } else {
            ceiling
        };
        match retry_after {
            Some(requested) => backoff.max(requested.min(self.max_delay)),
            None => backoff,
        }
    }
}

/// What kind of fetch this is; selects the time budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallClass {
    /// Index or metadata document: the whole fetch has a total budget (30 s).
    Metadata,
    /// Artifact data: bounded by idleness only (60 s without a byte), no total ceiling.
    Data,
}

/// Limits of the executor.
#[derive(Debug, Clone, Copy)]
pub struct ExecutorConfig {
    /// Total budget of a [`CallClass::Metadata`] fetch.
    pub metadata_timeout: Duration,
    /// Idle window for response heads and body chunks.
    pub idle_timeout: Duration,
    /// How long to wait for a concurrency permit before answering 503.
    pub permit_wait: Duration,
    /// Retry policy.
    pub retry: RetryPolicy,
    /// Circuit breaker tuning.
    pub breaker: BreakerConfig,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            metadata_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(60),
            permit_wait: Duration::from_secs(10),
            retry: RetryPolicy::default(),
            breaker: BreakerConfig::default(),
        }
    }
}

/// One logical upstream request.
#[derive(Debug, Clone)]
pub struct UpstreamCall {
    /// Repository the call is made for (concurrency cap key).
    pub repository: RepositoryId,
    /// Repository name, used as the metrics label.
    pub repo_name: String,
    /// `GET` or `HEAD`; other methods are sent once, never retried.
    pub method: Method,
    /// Target URL.
    pub url: Url,
    /// Request headers without credentials.
    pub headers: HeaderMap,
    /// Basic credentials, sent only to the origin of `url`.
    pub credentials: Option<UpstreamAuth>,
    /// Cap of concurrent upstream requests of this repository.
    pub max_concurrent: u32,
    /// Time budget class.
    pub class: CallClass,
}

/// The upstream answer after redirects, with a body that owns the concurrency permit.
pub struct UpstreamResult {
    /// Final status.
    pub status: StatusCode,
    /// Final response headers.
    pub headers: HeaderMap,
    /// Body stream with idle timeout (and deadline for metadata). Dropping it
    /// releases the concurrency permit and the connection.
    pub body: BoxStream<'static, Result<Bytes, UpstreamError>>,
}

impl std::fmt::Debug for UpstreamResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamResult")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Executes upstream calls with the full resilience stack. Shared by all repositories.
pub struct UpstreamExecutor {
    client: Arc<dyn UpstreamClient>,
    policy: Arc<UpstreamUrlPolicy>,
    config: ExecutorConfig,
    breakers: DashMap<String, Arc<CircuitBreaker>>,
    permits: DashMap<RepositoryId, (u32, Arc<Semaphore>)>,
}

impl UpstreamExecutor {
    /// Creates an executor over `client`.
    #[must_use]
    pub fn new(
        client: Arc<dyn UpstreamClient>,
        policy: Arc<UpstreamUrlPolicy>,
        config: ExecutorConfig,
    ) -> Self {
        Self {
            client,
            policy,
            config,
            breakers: DashMap::new(),
            permits: DashMap::new(),
        }
    }

    /// The URL policy (the proxy pipeline validates remote URLs with it too).
    #[must_use]
    pub fn policy(&self) -> &UpstreamUrlPolicy {
        &self.policy
    }

    /// The circuit breaker of an upstream host, if one was created yet.
    #[must_use]
    pub fn breaker_for_host(&self, host_key: &str) -> Option<Arc<CircuitBreaker>> {
        self.breakers
            .get(host_key)
            .map(|entry| Arc::clone(entry.value()))
    }

    fn breaker(&self, call: &UpstreamCall) -> Arc<CircuitBreaker> {
        let key = host_key(&call.url);
        let breaker = Arc::clone(
            self.breakers
                .entry(key)
                .or_insert_with(|| CircuitBreaker::new(self.config.breaker, call.repo_name.clone()))
                .value(),
        );
        // The breaker is per host, the `upstream_circuit_state{repo}` series is per
        // repository: every repository that calls the host must report it.
        breaker.register_repository(&call.repo_name);
        breaker
    }

    fn semaphore(&self, repository: RepositoryId, limit: u32) -> Arc<Semaphore> {
        let limit = limit.max(1);
        let mut entry = self
            .permits
            .entry(repository)
            .or_insert_with(|| (limit, Arc::new(Semaphore::new(limit as usize))));
        if entry.0 != limit {
            // The repository was reconfigured. Permits of the old semaphore drain
            // as their requests finish; new requests count against the new cap.
            *entry = (limit, Arc::new(Semaphore::new(limit as usize)));
        }
        Arc::clone(&entry.1)
    }

    async fn acquire(
        &self,
        semaphore: &Arc<Semaphore>,
    ) -> Result<OwnedSemaphorePermit, UpstreamError> {
        match tokio::time::timeout(
            self.config.permit_wait,
            Arc::clone(semaphore).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(permit),
            // Closed semaphore (never happens) or no permit in time: both mean "too busy".
            Ok(Err(_)) | Err(_) => Err(UpstreamError::Overloaded),
        }
    }

    /// Runs the call.
    ///
    /// The breaker slot and the concurrency permit are taken per attempt and are
    /// freed on every exit path; the permit of the returned response travels with
    /// its body stream.
    ///
    /// Cancel safe: dropping the future drops the permit and the breaker slot
    /// (an undecided probe frees its slot) and aborts the in-flight request.
    ///
    /// # Errors
    /// [`UpstreamError`] when no response could be obtained. An HTTP error status
    /// is *not* an error here: it is returned for the caller to interpret.
    pub async fn execute(&self, call: UpstreamCall) -> Result<UpstreamResult, UpstreamError> {
        self.policy.check_url(&call.url).map_err(|rejection| {
            metrics::upstream_error(&call.repo_name, "blocked");
            UpstreamError::Blocked(rejection)
        })?;
        let started = Instant::now();
        let deadline =
            (call.class == CallClass::Metadata).then(|| started + self.config.metadata_timeout);
        let breaker = self.breaker(&call);
        let semaphore = self.semaphore(call.repository, call.max_concurrent);
        let idempotent = call.method == Method::GET || call.method == Method::HEAD;
        let max_attempts = if idempotent {
            self.config.retry.max_attempts.max(1)
        } else {
            1
        };
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let Some(slot) = breaker.admit(Instant::now()) else {
                metrics::upstream_error(&call.repo_name, "circuit_open");
                return Err(UpstreamError::CircuitOpen);
            };
            let permit = match self.acquire(&semaphore).await {
                Ok(permit) => permit,
                Err(error) => {
                    // Waiting for our own cap says nothing about the upstream's health.
                    drop(slot);
                    metrics::upstream_error(&call.repo_name, "overloaded");
                    return Err(error);
                }
            };
            let began = Instant::now();
            let outcome = self.send_following_redirects(&call, deadline).await;
            let elapsed = began.elapsed();
            let can_retry = attempt < max_attempts;
            match outcome {
                Ok(response) => {
                    metrics::upstream_request(&call.repo_name, response.status.as_str(), elapsed);
                    let healthy = is_healthy(response.status);
                    slot.record(Instant::now(), healthy);
                    if !healthy {
                        metrics::upstream_error(&call.repo_name, "5xx");
                    }
                    if healthy || !can_retry || !is_retryable_status(response.status) {
                        return Ok(self.finish(response, permit, deadline));
                    }
                    let delay = self
                        .config
                        .retry
                        .delay_before_retry(attempt, retry_after(&response.headers));
                    if !fits(deadline, delay) {
                        return Ok(self.finish(response, permit, deadline));
                    }
                    drop(response);
                    drop(permit);
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    metrics::upstream_request(&call.repo_name, "error", elapsed);
                    metrics::upstream_error(&call.repo_name, error.metric_kind());
                    if matches!(error, UpstreamError::Blocked(_)) {
                        // A policy refusal is not an upstream failure; it must not open the breaker.
                        drop(slot);
                    } else {
                        slot.record(Instant::now(), false);
                    }
                    let delay = self.config.retry.delay_before_retry(attempt, None);
                    if error.is_retryable() && can_retry && fits(deadline, delay) {
                        drop(permit);
                        tokio::time::sleep(delay).await;
                    } else {
                        return Err(error);
                    }
                }
            }
        }
    }

    async fn send_following_redirects(
        &self,
        call: &UpstreamCall,
        deadline: Option<Instant>,
    ) -> Result<RawResponse, UpstreamError> {
        let origin = origin_of(&call.url);
        let mut url = call.url.clone();
        for hop in 0..=self.policy.max_redirects {
            let mut headers = call.headers.clone();
            if origin_of(&url) == origin {
                if let Some(credentials) = &call.credentials
                    && let Some(value) = basic_header(credentials)
                {
                    headers.insert(header::AUTHORIZATION, value);
                }
            } else {
                // A redirect to another origin must not carry credentials, whether they
                // came from the repository settings or were set by the format (a bearer
                // token for the upstream's own registry, for example).
                headers.remove(header::AUTHORIZATION);
                headers.remove(header::COOKIE);
                headers.remove(header::PROXY_AUTHORIZATION);
            }
            let wait = match deadline {
                Some(at) => at.saturating_duration_since(Instant::now()),
                None => self.config.idle_timeout,
            };
            let request = UpstreamRequest {
                method: call.method.clone(),
                url: url.clone(),
                headers,
            };
            let response = tokio::time::timeout(wait, self.client.send(request))
                .await
                .map_err(|_| UpstreamError::Timeout(TimeoutPhase::Headers))??;
            if !is_redirect(response.status) {
                return Ok(RawResponse {
                    status: response.status,
                    headers: response.headers,
                    body: response.body,
                });
            }
            let Some(location) = response
                .headers
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|text| url.join(text).ok())
            else {
                // A redirect without a usable target is a broken upstream, not content.
                return Err(UpstreamError::Protocol(
                    "redirect without a valid location".into(),
                ));
            };
            if hop == self.policy.max_redirects {
                return Err(UpstreamError::TooManyRedirects);
            }
            // Re-validate the new hop: scheme, host allowlist, literal addresses.
            // Name resolution of the new host is guarded inside the client.
            self.policy
                .check_url(&location)
                .map_err(UpstreamError::Blocked)?;
            url = location;
        }
        Err(UpstreamError::TooManyRedirects)
    }

    fn finish(
        &self,
        response: RawResponse,
        permit: OwnedSemaphorePermit,
        deadline: Option<Instant>,
    ) -> UpstreamResult {
        let guarded = IdleTimeout::new(response.body, self.config.idle_timeout, deadline)
            .map(|item| item.map_err(map_idle_error))
            .map(move |item| {
                // The closure owns the permit, so it lives exactly as long as the stream.
                let _held = &permit;
                item
            })
            .boxed();
        UpstreamResult {
            status: response.status,
            headers: response.headers,
            body: guarded,
        }
    }
}

struct RawResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: BoxStream<'static, Result<Bytes, UpstreamError>>,
}

fn map_idle_error(error: IdleError<UpstreamError>) -> UpstreamError {
    match error {
        IdleError::Idle(_) => UpstreamError::Timeout(TimeoutPhase::Idle),
        IdleError::Deadline => UpstreamError::Timeout(TimeoutPhase::Total),
        IdleError::Inner(inner) => inner,
    }
}

/// Key of the breaker map: `host:port`.
#[must_use]
pub fn host_key(url: &Url) -> String {
    format!(
        "{}:{}",
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default().unwrap_or(0)
    )
}

fn origin_of(url: &Url) -> (String, String, u16) {
    (
        url.scheme().to_owned(),
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default().unwrap_or(0),
    )
}

fn basic_header(credentials: &UpstreamAuth) -> Option<HeaderValue> {
    let raw = format!("{}:{}", credentials.username, credentials.password.expose());
    let mut value = HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(raw))).ok()?;
    value.set_sensitive(true);
    Some(value)
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// An answer that proves the upstream is alive: anything but 5xx, 429 and 408.
fn is_healthy(status: StatusCode) -> bool {
    !(status.is_server_error() || matches!(status.as_u16(), 429 | 408))
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// True when waiting `delay` still leaves room before the metadata deadline.
fn fits(deadline: Option<Instant>, delay: Duration) -> bool {
    deadline.is_none_or(|at| Instant::now() + delay < at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::ssrf::HostAllowlist;
    use crate::testing::{FakeUpstream, Reply};
    use rustryx_core::Secret;

    fn policy() -> Arc<UpstreamUrlPolicy> {
        Arc::new(UpstreamUrlPolicy {
            hosts: HostAllowlist::Only(vec![
                super::super::ssrf::HostPattern::parse("up.example").unwrap(),
                super::super::ssrf::HostPattern::parse("cdn.example").unwrap(),
            ]),
            ..UpstreamUrlPolicy::default()
        })
    }

    fn config() -> ExecutorConfig {
        ExecutorConfig {
            retry: RetryPolicy {
                max_attempts: 3,
                base_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(100),
                jitter: false,
            },
            permit_wait: Duration::from_millis(200),
            ..ExecutorConfig::default()
        }
    }

    fn executor(fake: &Arc<FakeUpstream>) -> UpstreamExecutor {
        UpstreamExecutor::new(fake.clone(), policy(), config())
    }

    fn call(url: &str) -> UpstreamCall {
        UpstreamCall {
            repository: RepositoryId::generate(),
            repo_name: "repo".into(),
            method: Method::GET,
            url: Url::parse(url).unwrap(),
            headers: HeaderMap::new(),
            credentials: None,
            max_concurrent: 4,
            class: CallClass::Data,
        }
    }

    async fn drain(result: UpstreamResult) -> Result<Vec<u8>, UpstreamError> {
        let mut body = result.body;
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(out)
    }

    #[tokio::test]
    async fn returns_the_body_of_a_successful_call() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::ok("hello")]);
        let result = executor(&fake)
            .execute(call("https://up.example/a"))
            .await
            .unwrap();
        assert_eq!(result.status, StatusCode::OK);
        assert_eq!(drain(result).await.unwrap(), b"hello");
        assert_eq!(fake.calls(), 1);
    }

    #[tokio::test]
    async fn must_not_call_upstream_for_a_blocked_url() {
        let fake = FakeUpstream::new();
        let exec = executor(&fake);
        for url in [
            "https://evil.example/a",
            "http://up.example/a",
            "https://127.0.0.1/a",
            "https://169.254.169.254/latest/meta-data",
        ] {
            let error = exec.execute(call(url)).await.unwrap_err();
            assert!(matches!(error, UpstreamError::Blocked(_)), "{url}");
        }
        assert_eq!(fake.calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_idempotent_requests_at_most_three_times() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::status(503)]);
        let result = executor(&fake)
            .execute(call("https://up.example/a"))
            .await
            .unwrap();
        assert_eq!(result.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(fake.calls(), 3, "one try and two retries, never more");
    }

    #[tokio::test(start_paused = true)]
    async fn recovers_when_a_retry_succeeds() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::status(502), Reply::ok("ok")]);
        let result = executor(&fake)
            .execute(call("https://up.example/a"))
            .await
            .unwrap();
        assert_eq!(result.status, StatusCode::OK);
        assert_eq!(fake.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_connect_errors_and_header_timeouts() {
        let fake = FakeUpstream::new();
        fake.route(
            "/a",
            vec![
                Reply::error(UpstreamError::Connect("refused".into())),
                Reply::error(UpstreamError::Timeout(TimeoutPhase::Headers)),
                Reply::ok("late"),
            ],
        );
        let result = executor(&fake)
            .execute(call("https://up.example/a"))
            .await
            .unwrap();
        assert_eq!(drain(result).await.unwrap(), b"late");
        assert_eq!(fake.calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_retry_non_idempotent_methods() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::status(503)]);
        let mut post = call("https://up.example/a");
        post.method = Method::POST;
        let result = executor(&fake).execute(post).await.unwrap();
        assert_eq!(result.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(fake.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_retry_client_errors_other_than_408_and_429() {
        for (code, calls) in [
            (404u16, 1usize),
            (400, 1),
            (403, 1),
            (401, 1),
            (429, 3),
            (408, 3),
        ] {
            let fake = FakeUpstream::new();
            fake.route("/a", vec![Reply::status(code)]);
            executor(&fake)
                .execute(call("https://up.example/a"))
                .await
                .unwrap();
            assert_eq!(fake.calls(), calls, "status {code}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_retry_a_policy_refusal() {
        let fake = FakeUpstream::new();
        fake.route(
            "/a",
            vec![Reply::error(UpstreamError::Blocked(
                super::super::ssrf::UrlRejection::NoHost,
            ))],
        );
        let error = executor(&fake)
            .execute(call("https://up.example/a"))
            .await
            .unwrap_err();
        assert!(matches!(error, UpstreamError::Blocked(_)));
        assert_eq!(fake.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn honours_retry_after_but_caps_it() {
        let fake = FakeUpstream::new();
        fake.route(
            "/a",
            vec![
                Reply::status(429).header("retry-after", "3600"),
                Reply::ok("x"),
            ],
        );
        let started = Instant::now();
        executor(&fake)
            .execute(call("https://up.example/a"))
            .await
            .unwrap();
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(100),
            "Retry-After raises the delay to the cap"
        );
        assert!(
            waited < Duration::from_secs(1),
            "but never beyond max_delay: {waited:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_when_headers_never_arrive() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::hang()]);
        let mut cfg = config();
        cfg.retry.max_attempts = 1;
        let exec = UpstreamExecutor::new(fake.clone(), policy(), cfg);
        let error = exec
            .execute(call("https://up.example/a"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            UpstreamError::Timeout(TimeoutPhase::Headers)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn metadata_calls_have_a_total_budget_data_calls_only_an_idle_window() {
        let fake = FakeUpstream::new();
        // 10 chunks, 20 s apart: 200 s in total, never idle for more than 20 s.
        fake.route(
            "/slow",
            vec![Reply::ok(vec![b'x'; 10]).chunked(1, Duration::from_secs(20))],
        );
        let data = executor(&fake)
            .execute(call("https://up.example/slow"))
            .await
            .unwrap();
        assert_eq!(
            drain(data).await.unwrap().len(),
            10,
            "a data stream has no total ceiling"
        );

        let mut meta = call("https://up.example/slow");
        meta.class = CallClass::Metadata;
        let result = executor(&fake).execute(meta).await.unwrap();
        let error = drain(result).await.unwrap_err();
        assert!(matches!(error, UpstreamError::Timeout(TimeoutPhase::Total)));
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_data_stream_fails_after_the_idle_window() {
        let fake = FakeUpstream::new();
        fake.route(
            "/stall",
            vec![Reply::ok(vec![b'x'; 4]).chunked(2, Duration::from_secs(61))],
        );
        let result = executor(&fake)
            .execute(call("https://up.example/stall"))
            .await
            .unwrap();
        let error = drain(result).await.unwrap_err();
        assert!(matches!(error, UpstreamError::Timeout(TimeoutPhase::Idle)));
    }

    #[tokio::test(start_paused = true)]
    async fn follows_validated_redirects_and_drops_credentials_across_origins() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::redirect(302, "https://cdn.example/blob")]);
        fake.route("/blob", vec![Reply::ok("data")]);
        let mut with_auth = call("https://up.example/a");
        with_auth.credentials = Some(UpstreamAuth {
            username: "svc".into(),
            password: Secret::new("s3cret"),
        });
        let result = executor(&fake).execute(with_auth).await.unwrap();
        assert_eq!(drain(result).await.unwrap(), b"data");
        let requests = fake.requests();
        assert!(requests[0].headers.contains_key(header::AUTHORIZATION));
        assert!(
            !requests[1].headers.contains_key(header::AUTHORIZATION),
            "must_not_send credentials to another origin"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_forward_format_set_credentials_to_another_origin() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::redirect(302, "https://cdn.example/blob")]);
        fake.route("/blob", vec![Reply::ok("data")]);
        let mut with_token = call("https://up.example/a");
        with_token.headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer registry-token"),
        );
        with_token
            .headers
            .insert(header::COOKIE, HeaderValue::from_static("s=1"));
        with_token
            .headers
            .insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        executor(&fake).execute(with_token).await.unwrap();
        let requests = fake.requests();
        assert!(requests[0].headers.contains_key(header::AUTHORIZATION));
        assert!(!requests[1].headers.contains_key(header::AUTHORIZATION));
        assert!(!requests[1].headers.contains_key(header::COOKIE));
        assert!(
            requests[1].headers.contains_key(header::ACCEPT),
            "harmless headers are kept"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn keeps_credentials_on_same_origin_redirects() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::redirect(301, "/b")]);
        fake.route("/b", vec![Reply::ok("same")]);
        let mut with_auth = call("https://up.example/a");
        with_auth.credentials = Some(UpstreamAuth {
            username: "svc".into(),
            password: Secret::new("s3cret"),
        });
        executor(&fake).execute(with_auth).await.unwrap();
        let requests = fake.requests();
        let header = requests[1].headers.get(header::AUTHORIZATION).unwrap();
        assert!(header.is_sensitive());
        assert_eq!(
            header.to_str().unwrap(),
            format!("Basic {}", STANDARD.encode("svc:s3cret"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_follow_redirects_to_unlisted_hosts_or_internal_addresses() {
        for target in [
            "https://evil.example/x",
            "http://cdn.example/x",
            "https://127.0.0.1/x",
            "https://169.254.169.254/latest",
        ] {
            let fake = FakeUpstream::new();
            fake.route("/a", vec![Reply::redirect(302, target)]);
            let error = executor(&fake)
                .execute(call("https://up.example/a"))
                .await
                .unwrap_err();
            assert!(matches!(error, UpstreamError::Blocked(_)), "{target}");
            assert_eq!(
                fake.calls(),
                1,
                "the blocked hop must not be requested: {target}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_follow_more_than_three_redirects() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::redirect(302, "/b")]);
        fake.route("/b", vec![Reply::redirect(302, "/c")]);
        fake.route("/c", vec![Reply::redirect(302, "/d")]);
        fake.route("/d", vec![Reply::redirect(302, "/e")]);
        fake.route("/e", vec![Reply::ok("too far")]);
        let error = executor(&fake)
            .execute(call("https://up.example/a"))
            .await
            .unwrap_err();
        assert!(matches!(error, UpstreamError::TooManyRedirects));
        assert_eq!(fake.calls(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_redirect_without_location_is_a_protocol_error() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::status(302)]);
        let error = executor(&fake)
            .execute(call("https://up.example/a"))
            .await
            .unwrap_err();
        assert!(matches!(error, UpstreamError::Protocol(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn the_concurrency_cap_answers_overloaded_instead_of_queueing_forever() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::ok("x")]);
        let exec = executor(&fake);
        let repository = RepositoryId::generate();
        let mut first = call("https://up.example/a");
        first.repository = repository;
        first.max_concurrent = 1;
        let held = exec.execute(first.clone()).await.unwrap();
        let error = exec.execute(first.clone()).await.unwrap_err();
        assert!(matches!(error, UpstreamError::Overloaded));
        // Releasing the body releases the permit.
        drop(held);
        assert!(exec.execute(first).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn must_not_hold_the_permit_while_backing_off() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::status(503), Reply::ok("x")]);
        let mut cfg = config();
        cfg.retry.base_delay = Duration::from_secs(1);
        cfg.retry.max_delay = Duration::from_secs(1);
        let exec = Arc::new(UpstreamExecutor::new(fake.clone(), policy(), cfg));
        let repository = RepositoryId::generate();
        let mut c = call("https://up.example/a");
        c.repository = repository;
        c.max_concurrent = 1;
        let first = tokio::spawn({
            let exec = Arc::clone(&exec);
            let c = c.clone();
            async move { exec.execute(c).await.map(|r| r.status) }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        // The first call is now sleeping between attempts. Its permit must be free.
        let mut other = c.clone();
        other.url = Url::parse("https://up.example/other").unwrap();
        fake.route("/other", vec![Reply::ok("y")]);
        assert!(exec.execute(other).await.is_ok());
        assert_eq!(first.await.unwrap().unwrap(), StatusCode::OK);
    }

    #[tokio::test(start_paused = true)]
    async fn breaker_opens_after_repeated_failures_and_stops_calling_upstream() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::status(500)]);
        let mut cfg = config();
        cfg.retry.max_attempts = 1;
        cfg.breaker.min_requests = 4;
        let exec = UpstreamExecutor::new(fake.clone(), policy(), cfg);
        for _ in 0..4 {
            exec.execute(call("https://up.example/a")).await.unwrap();
        }
        let before = fake.calls();
        let error = exec
            .execute(call("https://up.example/a"))
            .await
            .unwrap_err();
        assert!(matches!(error, UpstreamError::CircuitOpen));
        assert_eq!(
            fake.calls(),
            before,
            "an open breaker sends nothing upstream"
        );
        assert!(exec.breaker_for_host("up.example:443").is_some());
        // After the pause one probe goes out and a success closes the breaker.
        tokio::time::sleep(Duration::from_secs(31)).await;
        fake.route("/a", vec![Reply::ok("back")]);
        assert!(exec.execute(call("https://up.example/a")).await.is_ok());
        assert!(exec.execute(call("https://up.example/a")).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn not_found_does_not_count_as_an_upstream_failure() {
        let fake = FakeUpstream::new();
        fake.route("/a", vec![Reply::status(404)]);
        let mut cfg = config();
        cfg.breaker.min_requests = 2;
        let exec = UpstreamExecutor::new(fake.clone(), policy(), cfg);
        for _ in 0..10 {
            assert_eq!(
                exec.execute(call("https://up.example/a"))
                    .await
                    .unwrap()
                    .status,
                404
            );
        }
        assert_eq!(fake.calls(), 10);
    }

    #[test]
    fn backoff_is_exponential_capped_and_jittered_within_bounds() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(350),
            jitter: false,
        };
        assert_eq!(
            policy.delay_before_retry(1, None),
            Duration::from_millis(100)
        );
        assert_eq!(
            policy.delay_before_retry(2, None),
            Duration::from_millis(200)
        );
        assert_eq!(
            policy.delay_before_retry(3, None),
            Duration::from_millis(350)
        );
        assert_eq!(
            policy.delay_before_retry(30, None),
            Duration::from_millis(350)
        );
        let jittered = RetryPolicy {
            jitter: true,
            ..policy
        };
        for _ in 0..200 {
            assert!(jittered.delay_before_retry(2, None) <= Duration::from_millis(200));
        }
        assert_eq!(
            policy.delay_before_retry(1, Some(Duration::from_secs(1))),
            Duration::from_millis(350)
        );
    }

    #[test]
    fn defaults_match_the_normative_numbers() {
        let c = ExecutorConfig::default();
        assert_eq!(c.metadata_timeout, Duration::from_secs(30));
        assert_eq!(c.idle_timeout, Duration::from_secs(60));
        assert_eq!(c.retry.max_attempts, 3);
    }

    #[test]
    fn host_key_includes_port() {
        assert_eq!(
            host_key(&Url::parse("https://Up.Example/x").unwrap()),
            "up.example:443"
        );
        assert_eq!(
            host_key(&Url::parse("http://up.example:8080/x").unwrap()),
            "up.example:8080"
        );
    }
}
