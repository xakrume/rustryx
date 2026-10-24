//! `server` section.

use std::net::SocketAddr;
use std::time::Duration;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

/// HTTP listener, shutdown and edge policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Address of the single HTTP listener (API, formats, probes, metrics).
    pub listen_addr: SocketAddr,
    /// Public base URL (scheme and host, no path). Required for OIDC: the redirect
    /// URI is derived from it.
    pub base_url: Option<String>,
    /// Drain budget after SIGTERM in seconds (in-flight requests and background tasks).
    pub shutdown_timeout_secs: u64,
    /// Seconds to keep serving after readiness flipped to 503 and before the listener
    /// stops, so a load balancer notices. Platforms with a `preStop` hook can leave 0.
    pub drain_delay_secs: u64,
    /// Peers allowed to set `X-Forwarded-For` (reverse proxies, ingress).
    pub trusted_proxies: Vec<IpNet>,
    /// Exact browser origins allowed by CORS. Empty means no CORS headers are sent.
    pub cors_allowed_origins: Vec<String>,
    /// Refuse HTTP Basic credentials unless the request arrived over HTTPS. TLS ends in front
    /// of the node, so "arrived over HTTPS" means: the peer is in `trusted_proxies` and its
    /// `X-Forwarded-Proto` (or `Forwarded: proto=`) says `https`.
    pub require_https: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 8081)),
            base_url: None,
            shutdown_timeout_secs: 30,
            drain_delay_secs: 0,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            require_https: true,
        }
    }
}

impl ServerConfig {
    /// Drain budget as a duration.
    #[must_use]
    pub fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.shutdown_timeout_secs)
    }

    /// Pause between the readiness flip and closing the listener.
    #[must_use]
    pub fn drain_delay(&self) -> Duration {
        Duration::from_secs(self.drain_delay_secs)
    }
}
