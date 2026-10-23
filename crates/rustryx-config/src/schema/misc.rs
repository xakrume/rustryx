//! `lock`, `gc` and `observability` sections.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::secret::ConfigSecret;

/// Distributed lock backend. One per process life; there is no automatic switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockBackendKind {
    /// PostgreSQL advisory locks on a dedicated direct connection.
    Postgres,
    /// Redis `SET NX PX`.
    Redis,
}

/// `lock` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LockConfig {
    /// Backend for singleton jobs.
    pub backend: LockBackendKind,
    /// Redis URL, required for the `redis` backend. A secret: it may carry a password.
    pub redis_url: Option<ConfigSecret>,
    /// Lock TTL in seconds. Renewed every TTL/3.
    pub ttl_secs: u64,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            backend: LockBackendKind::Postgres,
            redis_url: None,
            ttl_secs: 30,
        }
    }
}

impl LockConfig {
    /// Lock TTL as a duration.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl_secs)
    }

    /// Renewal period: a third of the TTL, so two renewals can be missed.
    #[must_use]
    pub fn renew_interval(&self) -> Duration {
        self.ttl() / 3
    }
}

/// `gc` section: blob garbage collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GcConfig {
    /// Turns the collector on.
    pub enabled: bool,
    /// Seconds between runs.
    pub interval_secs: u64,
    /// A blob younger than this is never deleted. Must exceed `limits.request_timeout_secs`
    /// so that the blob of an upload still in progress is not collected.
    pub min_age_secs: u64,
    /// Blobs examined per batch.
    pub batch_size: u32,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 3600,
            min_age_secs: 86_400,
            batch_size: 1000,
        }
    }
}

/// Log line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// One JSON object per line.
    Json,
    /// Human readable lines for local development.
    Text,
}

/// OTLP transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OtlpProtocol {
    /// gRPC (usually port 4317).
    Grpc,
    /// HTTP with protobuf bodies (usually port 4318).
    Http,
}

/// `observability` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ObservabilityConfig {
    /// Log filter: comma separated `level` or `target=level` directives.
    pub log_level: String,
    /// Log format.
    pub log_format: LogFormat,
    /// OTLP collector endpoint. Unset means tracing export is off.
    pub otlp_endpoint: Option<String>,
    /// OTLP transport.
    pub otlp_protocol: OtlpProtocol,
    /// Head sampling ratio for traces without a sampled parent.
    pub otlp_sample_ratio: f64,
    /// `service.name` resource attribute.
    pub service_name: String,
    /// Serve `/metrics`.
    pub metrics_enabled: bool,
    /// Serve `/metrics` without a token.
    pub metrics_public: bool,
    /// Bearer token required by `/metrics` unless `metrics_public` is set.
    pub metrics_token: Option<ConfigSecret>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_owned(),
            log_format: LogFormat::Json,
            otlp_endpoint: None,
            otlp_protocol: OtlpProtocol::Grpc,
            otlp_sample_ratio: 0.1,
            service_name: "rustryx".to_owned(),
            metrics_enabled: true,
            metrics_public: false,
            metrics_token: None,
        }
    }
}
