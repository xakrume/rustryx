//! Typed configuration of Rustryx: schema, layered loading and validation.
//!
//! Layers, from lowest to highest priority: built-in defaults, a YAML file
//! (`--config` or `RUSTRYX_CONFIG`), environment variables. Every key `a.b.c` is
//! overridden by `RUSTRYX_A_B_C`; secrets may instead come from the file named by
//! `RUSTRYX_A_B_C_FILE` (a mounted Kubernetes secret). All problems are collected
//! and reported together before any port is opened.
//!
//! The crate performs its file reads synchronously on purpose: it runs once,
//! before the async runtime exists.

// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod env;
mod error;
pub mod keys;
mod load;
mod overlay;
pub mod reference;
mod resources;
mod schema;
mod secret;
mod tree;
mod validate;

#[cfg(test)]
mod tests;

pub use env::{EnvLookup, EnvSource, MapEnv, ProcessEnv};
pub use error::{ConfigError, ConfigIssue, ConfigIssues, EXIT_CODE_CONFIG, IssueOrigin};
pub use load::{CONFIG_PATH_VAR, Loaded, load, load_without_rules, resolve_config_path};
pub use resources::{logical_cpus, physical_cores};
pub use schema::{
    AppConfig, AuthConfig, BlobConfig, BlobKind, BootstrapAdminConfig, DatabaseConfig, Environment,
    GcConfig, GroupStrategy, JitMode, LdapConfig, LimitsConfig, LockBackendKind, LockConfig,
    LogFormat, ObservabilityConfig, OidcConfig, OtlpProtocol, RateLimitConfig, ServerConfig,
    SessionConfig, UpstreamConfig,
};
pub use secret::{ConfigSecret, REDACTED};

impl AppConfig {
    /// Checks the rules of a configuration built in code (tests, tools). Loading
    /// from file and environment already does this.
    ///
    /// # Errors
    /// Every violated rule, in one [`ConfigIssues`].
    pub fn validate(&self) -> Result<Vec<String>, ConfigIssues> {
        let report = validate::run(self);
        if report.issues.is_empty() {
            Ok(report.warnings)
        } else {
            Err(ConfigIssues(report.issues))
        }
    }

    /// The effective configuration as YAML with every secret replaced by
    /// `[REDACTED]` (or `null` when unset).
    ///
    /// # Errors
    /// [`ConfigError::Render`] if serialization fails.
    pub fn to_redacted_yaml(&self) -> Result<String, ConfigError> {
        serde_yaml_ng::to_string(self).map_err(|e| ConfigError::Render(e.to_string()))
    }
}
