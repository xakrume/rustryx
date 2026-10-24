//! Typed configuration schema.
//!
//! Every section is `deny_unknown_fields`: a typo in a security key such as
//! `anonymous_enable` must stop the process, not silently keep the default.
//! Adding a key therefore needs a change here, in [`crate::keys::KEYS`] and in the
//! docs (three tests keep the three in step).

mod auth;
mod blob;
mod database;
mod egress;
mod limits;
mod misc;
mod platform;
mod server;
mod upstream;

use serde::{Deserialize, Serialize};

pub use auth::{
    AuthConfig, BootstrapAdminConfig, GroupStrategy, JitMode, LdapConfig, OidcConfig, SessionConfig,
};
pub use blob::{BlobConfig, BlobKind};
pub use database::DatabaseConfig;
pub use egress::{EgressConfig, EgressPolicyConfig, EgressProxyConfig, EgressSmtpConfig};
pub use limits::{LimitsConfig, RateLimitConfig};
pub use misc::{
    GcConfig, LockBackendKind, LockConfig, LogFormat, ObservabilityConfig, OtlpProtocol,
};
pub use platform::{DownloadsConfig, OutboxConfig, TasksConfig};
pub use server::ServerConfig;
pub use upstream::UpstreamConfig;

/// Deployment environment. Controls how strict credential checks are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /// Refuses default, empty and well-known weak credentials. The default, so
    /// that forgetting to set the key never weakens a deployment.
    Production,
    /// Relaxes credential strictness for local work.
    Development,
}

/// The complete, typed configuration of a Rustryx node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AppConfig {
    /// Deployment environment.
    pub environment: Environment,
    /// HTTP listener and shutdown.
    pub server: ServerConfig,
    /// PostgreSQL connection and pool.
    pub database: DatabaseConfig,
    /// Default blob store seed.
    pub blob: BlobConfig,
    /// Authentication and identity providers.
    pub auth: AuthConfig,
    /// Distributed lock backend.
    pub lock: LockConfig,
    /// Request, concurrency and rate limits.
    pub limits: LimitsConfig,
    /// Outbound (proxy repository) policy.
    pub upstream: UpstreamConfig,
    /// Blob garbage collection.
    pub gc: GcConfig,
    /// Logging, metrics and tracing.
    pub observability: ObservabilityConfig,
    /// Background task framework.
    pub tasks: TasksConfig,
    /// Event outbox.
    pub outbox: OutboxConfig,
    /// Download counters.
    pub downloads: DownloadsConfig,
    /// Outbound policy of registries, webhooks, scanners, mail and the trust store probe.
    pub egress: EgressConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            environment: Environment::Production,
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            blob: BlobConfig::default(),
            auth: AuthConfig::default(),
            lock: LockConfig::default(),
            limits: LimitsConfig::default(),
            upstream: UpstreamConfig::default(),
            gc: GcConfig::default(),
            observability: ObservabilityConfig::default(),
            tasks: TasksConfig::default(),
            outbox: OutboxConfig::default(),
            downloads: DownloadsConfig::default(),
            egress: EgressConfig::default(),
        }
    }
}

impl AppConfig {
    /// True when `environment` is `production`.
    #[must_use]
    pub fn is_production(&self) -> bool {
        self.environment == Environment::Production
    }
}
