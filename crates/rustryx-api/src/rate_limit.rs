//! Per-client rate limiting: a token bucket per user, or per address for
//! anonymous callers.
//!
//! The key is the authenticated username when there is one, otherwise the client
//! address resolved by `client_ip` (which only believes `X-Forwarded-For` from
//! trusted proxies). A client therefore cannot get a fresh bucket by sending a
//! different forwarded address, and one noisy user does not slow the others.
//!
//! Memory is bounded: buckets live in a size-capped cache and idle ones are
//! evicted. Evicting a bucket that has been idle long enough to be full loses
//! nothing, since a missing bucket starts full anyway.

use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use moka::future::Cache;
use rustryx_core::identity::Principal;
use rustryx_observability::MetricsHandle;
use tokio::time::Instant;

use crate::client_ip::ClientIp;
use crate::error::ApiError;

/// Most distinct clients tracked at once.
const MAX_TRACKED_CLIENTS: u64 = 100_000;
/// Extra idle time before a full bucket is dropped from the cache.
const IDLE_MARGIN: Duration = Duration::from_secs(60);

/// What a bucket is keyed by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientKey {
    /// An authenticated user.
    User(Arc<str>),
    /// An anonymous caller at this address.
    Address(IpAddr),
    /// No peer address is known (in-process use).
    Unknown,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

/// Token-bucket limiter with a bounded, self-cleaning key space.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: Cache<ClientKey, Arc<Mutex<Bucket>>>,
    rate: f64,
    burst: f64,
}

impl RateLimiter {
    /// `requests_per_second` sustained, `burst` at once (both at least 1).
    #[must_use]
    pub fn new(requests_per_second: u32, burst: u32) -> Self {
        Self::from_rate(f64::from(requests_per_second.max(1)), burst)
    }

    /// `per_minute` sustained, `burst` at once (both at least 1). For counters that must
    /// refill slowly, such as failed credential checks.
    #[must_use]
    pub fn per_minute(per_minute: u32, burst: u32) -> Self {
        Self::from_rate(f64::from(per_minute.max(1)) / 60.0, burst)
    }

    fn from_rate(rate: f64, burst: u32) -> Self {
        let burst = f64::from(burst.max(1));
        let refill = Duration::from_secs_f64(burst / rate);
        Self {
            buckets: Cache::builder()
                .max_capacity(MAX_TRACKED_CLIENTS)
                .time_to_idle(refill + IDLE_MARGIN)
                .build(),
            rate,
            burst,
        }
    }

    /// How long until `key` has a token again, without taking one. `None` when it has
    /// one now (or was never seen).
    pub async fn wait_time(&self, key: &ClientKey) -> Option<Duration> {
        let bucket = self.buckets.get(key).await?;
        // The guard lives only inside this block and never across an await.
        let mut bucket = bucket.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate).min(self.burst);
        bucket.updated = now;
        (bucket.tokens < 1.0).then(|| Duration::from_secs_f64((1.0 - bucket.tokens) / self.rate))
    }

    /// Takes one token for `key` whether or not one is available, so that an event that
    /// already happened (a failed credential check) is always counted.
    pub async fn charge(&self, key: ClientKey) {
        let burst = self.burst;
        let bucket = self
            .buckets
            .get_with(key, async move {
                Arc::new(Mutex::new(Bucket {
                    tokens: burst,
                    updated: Instant::now(),
                }))
            })
            .await;
        // The guard lives only inside this block and never across an await.
        let mut bucket = bucket.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate).min(self.burst);
        bucket.updated = now;
        bucket.tokens = (bucket.tokens - 1.0).max(0.0);
    }

    /// Takes one token for `key`.
    ///
    /// # Errors
    /// The time to wait until a token is available.
    pub async fn check(&self, key: ClientKey) -> Result<(), Duration> {
        let burst = self.burst;
        let bucket = self
            .buckets
            .get_with(key, async move {
                Arc::new(Mutex::new(Bucket {
                    tokens: burst,
                    updated: Instant::now(),
                }))
            })
            .await;
        // The guard lives only inside this block and never across an await.
        let mut bucket = bucket.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate).min(self.burst);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / self.rate))
        }
    }
}

/// State of the middleware.
#[derive(Clone)]
pub(crate) struct LimitState {
    pub limiter: Arc<RateLimiter>,
    pub metrics: Option<MetricsHandle>,
}

fn key_of(request: &Request) -> ClientKey {
    match request.extensions().get::<Principal>() {
        Some(principal) if !principal.is_anonymous() => {
            ClientKey::User(Arc::from(principal.username.as_str()))
        }
        _ => request
            .extensions()
            .get::<ClientIp>()
            .and_then(|c| c.0)
            .map_or(ClientKey::Unknown, ClientKey::Address),
    }
}

/// Runs inside authentication, so the principal is known.
pub(crate) async fn rate_limit(
    State(state): State<LimitState>,
    request: Request,
    next: Next,
) -> Response {
    match state.limiter.check(key_of(&request)).await {
        Ok(()) => next.run(request).await,
        Err(wait) => {
            if let Some(metrics) = &state.metrics {
                metrics.record_shed("rate_limit");
            }
            ApiError::new(
                http::StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Too many requests, slow down",
            )
            .with_retry_after(wait)
            .into_response()
        }
    }
}

/// State of the failed-credential guard.
#[derive(Clone)]
pub(crate) struct FailureGuardState {
    pub limiter: Arc<RateLimiter>,
    pub metrics: Option<MetricsHandle>,
}

/// Failed credential checks one address may make before it is refused (burst).
pub(crate) const AUTH_FAILURE_BURST: u32 = 10;
/// Failed credential checks per minute an address regains afterwards.
pub(crate) const AUTH_FAILURE_PER_MINUTE: u32 = 10;

/// Wraps `authenticate` from the outside, keyed on the client address.
///
/// The per-user limiter inside authentication only sees callers that already proved who
/// they are, so a client guessing tokens or passwords was never counted. This guard
/// charges the address for every request that carried credentials and was answered 401,
/// and refuses further credentialed requests from it with 429 until the bucket refills.
/// Requests without an `Authorization` header are never charged or refused: clients
/// such as docker, maven and npm first send an anonymous request to learn the challenge,
/// and a shared NAT address must not lock that flow out.
pub(crate) async fn auth_failure_guard(
    State(state): State<FailureGuardState>,
    request: Request,
    next: Next,
) -> Response {
    if !request.headers().contains_key(http::header::AUTHORIZATION) {
        return next.run(request).await;
    }
    let key = request
        .extensions()
        .get::<ClientIp>()
        .and_then(|c| c.0)
        .map_or(ClientKey::Unknown, ClientKey::Address);
    if let Some(wait) = state.limiter.wait_time(&key).await {
        if let Some(metrics) = &state.metrics {
            metrics.record_shed("auth_failure_limit");
        }
        return ApiError::new(
            http::StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many failed authentication attempts, slow down",
        )
        .with_retry_after(wait)
        .into_response();
    }
    let response = next.run(request).await;
    if response.status() == http::StatusCode::UNAUTHORIZED {
        state.limiter.charge(key).await;
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str) -> ClientKey {
        ClientKey::User(Arc::from(name))
    }

    #[tokio::test(start_paused = true)]
    async fn charge_counts_events_that_already_happened_and_wait_time_peeks() {
        let limiter = RateLimiter::per_minute(60, 2);
        let key = user("a");
        assert_eq!(limiter.wait_time(&key).await, None, "never seen");
        limiter.charge(key.clone()).await;
        assert_eq!(limiter.wait_time(&key).await, None, "one token left");
        limiter.charge(key.clone()).await;
        limiter.charge(key.clone()).await;
        let wait = limiter.wait_time(&key).await.expect("exhausted");
        assert!(
            wait > Duration::ZERO && wait <= Duration::from_secs(1),
            "{wait:?}"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            limiter.wait_time(&key).await,
            None,
            "one per second refilled"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_is_allowed_then_requests_wait_for_refill() {
        let limiter = RateLimiter::new(2, 5);
        for _ in 0..5 {
            assert!(limiter.check(user("a")).await.is_ok());
        }
        let wait = limiter.check(user("a")).await.expect_err("burst is spent");
        assert!(
            wait > Duration::ZERO && wait <= Duration::from_millis(500),
            "{wait:?}"
        );

        tokio::time::advance(Duration::from_millis(500)).await;
        assert!(limiter.check(user("a")).await.is_ok(), "one token refilled");
        assert!(limiter.check(user("a")).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn refill_never_exceeds_the_burst() {
        let limiter = RateLimiter::new(10, 3);
        for _ in 0..3 {
            assert!(limiter.check(user("a")).await.is_ok());
        }
        tokio::time::advance(Duration::from_secs(3600)).await;
        for _ in 0..3 {
            assert!(limiter.check(user("a")).await.is_ok());
        }
        assert!(
            limiter.check(user("a")).await.is_err(),
            "an idle hour does not bank more than the burst"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn clients_have_independent_buckets() {
        let limiter = RateLimiter::new(1, 1);
        assert!(limiter.check(user("a")).await.is_ok());
        assert!(limiter.check(user("a")).await.is_err());
        assert!(limiter.check(user("b")).await.is_ok());
        let ip: IpAddr = "192.0.2.1".parse().expect("ip");
        assert!(limiter.check(ClientKey::Address(ip)).await.is_ok());
        assert!(limiter.check(ClientKey::Unknown).await.is_ok());
        assert!(limiter.check(ClientKey::Unknown).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn zero_settings_are_clamped_to_one() {
        let limiter = RateLimiter::new(0, 0);
        assert!(limiter.check(user("a")).await.is_ok());
        assert!(limiter.check(user("a")).await.is_err());
    }
}
