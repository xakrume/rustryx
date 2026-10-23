//! Rules for `server` and `upstream`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;

use super::Report;
use crate::AppConfig;

pub(super) fn server(config: &AppConfig, report: &mut Report) {
    let server = &config.server;
    report.range(
        "server.shutdown_timeout_secs",
        server.shutdown_timeout_secs,
        1,
        3600,
    );
    report.range("server.drain_delay_secs", server.drain_delay_secs, 0, 300);

    if let Some(raw) = &server.base_url {
        base_url(config, raw, report);
    } else if config.auth.oidc.enabled {
        report.err(
            "server.base_url",
            "is required when auth.oidc.enabled (the redirect URI is derived from it): set RUSTRYX_SERVER_BASE_URL",
        );
    }

    for (i, net) in server.trusted_proxies.iter().enumerate() {
        // A /0 trusts every peer to choose its own client address, which defeats the
        // login guard (it counts failures per source address) and falsifies audit logs.
        if net.prefix_len() == 0 {
            report.err(
                "server.trusted_proxies",
                format!("entry {i} trusts every address (prefix length 0)"),
            );
        }
    }
    for raw in &server.cors_allowed_origins {
        cors_origin(config, raw, report);
    }
}

fn base_url(config: &AppConfig, raw: &str, report: &mut Report) {
    let Some(url) = report.url("server.base_url", raw) else {
        return;
    };
    let key = "server.base_url";
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        report.err(key, "must be an http(s) URL with a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        report.err(key, "must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        report.err(key, "must not contain a query or fragment");
    }
    if config.is_production() && config.auth.oidc.enabled && url.scheme() != "https" {
        // Session and state cookies are `Secure`; over http the login would not work
        // and a downgraded redirect URI would expose the authorization code.
        report.err(key, "must use https in production when OIDC is enabled");
    }
}

fn cors_origin(config: &AppConfig, raw: &str, report: &mut Report) {
    let key = "server.cors_allowed_origins";
    if raw.contains('*') {
        // The allowlist is exact on purpose: a wildcard together with credentialed
        // requests, or an echoed Origin, lets any site read authenticated responses.
        report.err(key, "wildcards are not allowed, list exact origins");
        return;
    }
    let Some(url) = report.url(key, raw) else {
        return;
    };
    let shape_ok = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && !raw.ends_with('/');
    if !shape_ok {
        report.err(
            key,
            "each entry must look like https://host[:port] without a path",
        );
    } else if config.is_production() && url.scheme() == "http" {
        report.warn(key, "an http origin is allowed in production");
    }
}

pub(super) fn upstream(config: &AppConfig, report: &mut Report) {
    let up = &config.upstream;
    report.range("upstream.max_redirects", u64::from(up.max_redirects), 0, 3);
    report.range("upstream.max_attempts", u64::from(up.max_attempts), 1, 3);
    report.range(
        "upstream.connect_timeout_secs",
        up.connect_timeout_secs,
        1,
        60,
    );
    report.range(
        "upstream.metadata_timeout_secs",
        up.metadata_timeout_secs,
        1,
        300,
    );
    report.range("upstream.idle_timeout_secs", up.idle_timeout_secs, 1, 600);

    schemes(config, report);
    host_lists(config, report);
    for net in &up.allow_private_cidrs {
        private_cidr(net, report);
    }
}

fn schemes(config: &AppConfig, report: &mut Report) {
    let up = &config.upstream;
    let key = "upstream.allowed_schemes";
    for scheme in &up.allowed_schemes {
        if !matches!(scheme.as_str(), "http" | "https") {
            report.err(key, "only `http` and `https` are supported");
        }
    }
    let https = up.allowed_schemes.iter().any(|s| s == "https");
    let http = up.allowed_schemes.iter().any(|s| s == "http");
    if !https {
        report.err(key, "must include `https`");
    }
    if http && up.allow_insecure_http_hosts.is_empty() {
        report.err(
            key,
            "`http` needs the hosts it applies to in upstream.allow_insecure_http_hosts",
        );
    }
    if !http && !up.allow_insecure_http_hosts.is_empty() {
        report.err(
            "upstream.allow_insecure_http_hosts",
            "has entries but `http` is not in upstream.allowed_schemes",
        );
    }
    if http && config.is_production() {
        report.warn(key, "plain http upstreams are allowed in production");
    }
}

fn host_lists(config: &AppConfig, report: &mut Report) {
    let up = &config.upstream;
    for host in &up.allow_insecure_http_hosts {
        if !is_plain_host(host) {
            report.err(
                "upstream.allow_insecure_http_hosts",
                "entries must be host names or IP addresses without scheme, port or wildcard",
            );
        }
    }
    for host in &up.allowed_hosts {
        let name = host.strip_prefix("*.").unwrap_or(host);
        if !is_plain_host(name) {
            report.err(
                "upstream.allowed_hosts",
                "entries must be host names, or `*.example.com` patterns",
            );
        }
    }
}

/// Host name or IP literal: no scheme, port, path, wildcard or whitespace. A colon
/// is accepted only inside an IPv6 literal, never as a port separator.
fn is_plain_host(value: &str) -> bool {
    let bare = value.trim_start_matches('[').trim_end_matches(']');
    if bare.contains(':') {
        return bare.parse::<Ipv6Addr>().is_ok();
    }
    !bare.is_empty()
        && bare.len() <= 253
        && bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        && !bare.contains("..")
}

/// Rejects allowlist ranges that would legitimize loopback or the cloud metadata
/// endpoint. The allowlist exists for internal mirrors; if it could cover those
/// addresses, anyone who can create a proxy repository could read instance
/// credentials through it.
fn private_cidr(net: &IpNet, report: &mut Report) {
    let key = "upstream.allow_private_cidrs";
    let forbidden: [IpAddr; 4] = [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
        IpAddr::V6(Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)),
    ];
    if net.prefix_len() == 0 {
        report.err(key, "a /0 range disables the SSRF filter");
    } else if forbidden.iter().any(|ip| net.contains(ip)) {
        report.err(
            key,
            "must not cover loopback or the cloud metadata address (169.254.169.254)",
        );
    }
}
