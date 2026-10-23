//! `limits` section.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Per-client rate limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RateLimitConfig {
    /// Turns the limiter on.
    pub enabled: bool,
    /// Sustained requests per second per user or client address.
    pub requests_per_second: u32,
    /// Burst size.
    pub burst: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            requests_per_second: 50,
            burst: 100,
        }
    }
}

/// Request, concurrency and resource limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    /// Largest JSON/metadata request body in MiB.
    pub max_body_mb: u64,
    /// Largest artifact body in bytes. Larger uploads get 413.
    pub max_blob_bytes: u64,
    /// Ceiling for producing the response head of one request.
    pub request_timeout_secs: u64,
    /// Time a client may take to send request headers (slowloris guard).
    pub header_read_timeout_secs: u64,
    /// Time a request or response body may stay idle between chunks.
    pub body_idle_timeout_secs: u64,
    /// Open connections per node.
    pub max_connections: u32,
    /// Concurrently processed requests per node. Beyond this requests get 503.
    pub max_in_flight_requests: u32,
    /// Permits for blocking work (archive unpacking, large XML). 0 means 2 x CPUs.
    pub blocking_permits: u32,
    /// Permits for password hashing. 0 means the number of physical cores.
    pub password_permits: u32,
    /// How long a request waits for a blocking permit before it gets 503.
    pub permit_wait_ms: u64,
    /// Rate limit.
    pub rate_limit: RateLimitConfig,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_body_mb: 10,
            max_blob_bytes: 5 * 1024 * 1024 * 1024,
            request_timeout_secs: 1800,
            header_read_timeout_secs: 10,
            body_idle_timeout_secs: 60,
            max_connections: 20_000,
            max_in_flight_requests: 10_000,
            blocking_permits: 0,
            password_permits: 0,
            permit_wait_ms: 1000,
            rate_limit: RateLimitConfig::default(),
        }
    }
}

impl LimitsConfig {
    /// Metadata body limit in bytes.
    #[must_use]
    pub fn max_body_bytes(&self) -> u64 {
        self.max_body_mb.saturating_mul(1024 * 1024)
    }

    /// Request timeout as a duration.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    /// Header read timeout as a duration.
    #[must_use]
    pub fn header_read_timeout(&self) -> Duration {
        Duration::from_secs(self.header_read_timeout_secs)
    }

    /// Body idle timeout as a duration.
    #[must_use]
    pub fn body_idle_timeout(&self) -> Duration {
        Duration::from_secs(self.body_idle_timeout_secs)
    }

    /// Permit wait deadline as a duration.
    #[must_use]
    pub fn permit_wait(&self) -> Duration {
        Duration::from_millis(self.permit_wait_ms)
    }

    /// Blocking permits after resolving the `0 = auto` default (2 x CPUs).
    #[must_use]
    pub fn effective_blocking_permits(&self) -> usize {
        if self.blocking_permits == 0 {
            crate::resources::logical_cpus().saturating_mul(2)
        } else {
            self.blocking_permits as usize
        }
    }

    /// Password permits after resolving the `0 = auto` default (physical cores).
    #[must_use]
    pub fn effective_password_permits(&self) -> usize {
        if self.password_permits == 0 {
            crate::resources::physical_cores()
        } else {
            self.password_permits as usize
        }
    }
}
