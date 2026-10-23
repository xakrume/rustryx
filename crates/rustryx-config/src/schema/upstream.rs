//! `upstream` section: outbound policy for proxy repositories.

use std::time::Duration;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

/// Policy applied to every proxy repository `remote_url` and to the HTTP client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UpstreamConfig {
    /// URL schemes a `remote_url` may use. `http` additionally needs the host in
    /// `allow_insecure_http_hosts`.
    pub allowed_schemes: Vec<String>,
    /// Hosts that may be reached over plain `http`.
    pub allow_insecure_http_hosts: Vec<String>,
    /// When not empty, only these hosts (exact, or `*.example.com`) may be upstreams.
    pub allowed_hosts: Vec<String>,
    /// Internal ranges that may be used as upstreams despite the SSRF filter.
    pub allow_private_cidrs: Vec<IpNet>,
    /// Redirects followed per request (each one is checked again).
    pub max_redirects: u32,
    /// Connect timeout.
    pub connect_timeout_secs: u64,
    /// Timeout for metadata requests.
    pub metadata_timeout_secs: u64,
    /// Idle timeout of a streaming body.
    pub idle_timeout_secs: u64,
    /// Attempts for GET and HEAD, including the first.
    pub max_attempts: u32,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            allowed_schemes: vec!["https".to_owned()],
            allow_insecure_http_hosts: Vec::new(),
            allowed_hosts: Vec::new(),
            allow_private_cidrs: Vec::new(),
            max_redirects: 3,
            connect_timeout_secs: 3,
            metadata_timeout_secs: 30,
            idle_timeout_secs: 60,
            max_attempts: 3,
        }
    }
}

impl UpstreamConfig {
    /// Connect timeout as a duration.
    #[must_use]
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    /// Metadata timeout as a duration.
    #[must_use]
    pub fn metadata_timeout(&self) -> Duration {
        Duration::from_secs(self.metadata_timeout_secs)
    }

    /// Streaming idle timeout as a duration.
    #[must_use]
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }
}
