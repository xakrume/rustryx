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
        let rate = f64::from(requests_per_second.max(1));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str) -> ClientKey {
        ClientKey::User(Arc::from(name))
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
