//! `egress` section: the outbound policy of every purpose except proxy repositories
//! (those keep `upstream.*`).
//!
//! One policy per purpose, so that a webhook is never allowed what an internal mirror is.
//! These keys are file or environment configuration and not API editable settings, on
//! purpose: they are the security boundary, and an administrator who can edit settings
//! through the API must not be able to widen them. Loopback, link local, metadata,
//! multicast, unspecified and broadcast ranges can never be allowlisted.

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

/// Policy of one outbound purpose that speaks HTTP.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EgressPolicyConfig {
    /// URL schemes the purpose may use. `http` additionally needs the host in
    /// `allow_insecure_http_hosts`.
    pub allowed_schemes: Vec<String>,
    /// Hosts reachable over plain `http`.
    pub allow_insecure_http_hosts: Vec<String>,
    /// When not empty, only these hosts (exact, or `*.example.com`) may be targets.
    pub allowed_hosts: Vec<String>,
    /// Destination ports. A URL naming another port is refused.
    pub allowed_ports: Vec<u16>,
    /// Internal ranges allowed despite the SSRF filter.
    pub allow_private_cidrs: Vec<IpNet>,
}

impl Default for EgressPolicyConfig {
    fn default() -> Self {
        Self {
            allowed_schemes: vec!["https".to_owned()],
            allow_insecure_http_hosts: Vec::new(),
            allowed_hosts: Vec::new(),
            allowed_ports: vec![443],
            allow_private_cidrs: Vec::new(),
        }
    }
}

/// Policy of the mail relay: no URL, so no scheme.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EgressSmtpConfig {
    /// When not empty, only these hosts may be the relay.
    pub allowed_hosts: Vec<String>,
    /// Relay ports: 465 (implicit TLS) and 587 (submission with `StartTLS`) by default.
    pub allowed_ports: Vec<u16>,
    /// Internal ranges allowed despite the SSRF filter (an internal relay).
    pub allow_private_cidrs: Vec<IpNet>,
}

impl Default for EgressSmtpConfig {
    fn default() -> Self {
        Self {
            allowed_hosts: Vec::new(),
            allowed_ports: vec![465, 587],
            allow_private_cidrs: Vec::new(),
        }
    }
}

/// Which hosts the outbound HTTP proxy setting may name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EgressProxyConfig {
    /// Hosts an administrator may configure as the outbound proxy. Empty means the proxy
    /// setting is refused.
    pub allowed_hosts: Vec<String>,
}

/// `egress` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EgressConfig {
    /// Registry endpoints and replication.
    pub registries: EgressPolicyConfig,
    /// Webhook deliveries (never redirected; internal ranges empty by default).
    pub webhooks: EgressPolicyConfig,
    /// HTTP scanner adapters and data set downloads.
    pub scanners: EgressPolicyConfig,
    /// The mail relay.
    pub smtp: EgressSmtpConfig,
    /// Certificate fetch helper of the trust store (public addresses only by default).
    pub probe: EgressPolicyConfig,
    /// The outbound proxy setting.
    pub proxy: EgressProxyConfig,
}
