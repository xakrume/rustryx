//! Rules for `limits` and `observability`.

use super::Report;
use crate::AppConfig;

pub(super) fn limits(config: &AppConfig, report: &mut Report) {
    let l = &config.limits;
    report.range("limits.max_body_mb", l.max_body_mb, 1, 1024);
    report.range("limits.max_blob_bytes", l.max_blob_bytes, 1, u64::MAX);
    report.range(
        "limits.request_timeout_secs",
        l.request_timeout_secs,
        1,
        86_400,
    );
    report.range(
        "limits.header_read_timeout_secs",
        l.header_read_timeout_secs,
        1,
        300,
    );
    report.range(
        "limits.body_idle_timeout_secs",
        l.body_idle_timeout_secs,
        1,
        600,
    );
    report.range(
        "limits.max_connections",
        u64::from(l.max_connections),
        1,
        1_000_000,
    );
    report.range(
        "limits.max_in_flight_requests",
        u64::from(l.max_in_flight_requests),
        1,
        1_000_000,
    );
    report.range(
        "limits.blocking_permits",
        u64::from(l.blocking_permits),
        0,
        4096,
    );
    report.range(
        "limits.password_permits",
        u64::from(l.password_permits),
        0,
        256,
    );
    report.range("limits.permit_wait_ms", l.permit_wait_ms, 1, 60_000);
    if l.rate_limit.enabled {
        report.range(
            "limits.rate_limit.requests_per_second",
            u64::from(l.rate_limit.requests_per_second),
            1,
            1_000_000,
        );
        report.range(
            "limits.rate_limit.burst",
            u64::from(l.rate_limit.burst),
            1,
            10_000_000,
        );
    }
}

pub(super) fn observability(config: &AppConfig, report: &mut Report) {
    let obs = &config.observability;
    log_level(&obs.log_level, report);

    if obs.service_name.trim().is_empty() || obs.service_name.len() > 64 {
        report.err("observability.service_name", "must be 1 to 64 characters");
    }
    if !(0.0..=1.0).contains(&obs.otlp_sample_ratio) {
        report.err("observability.otlp_sample_ratio", "must be between 0 and 1");
    }
    if let Some(raw) = obs.otlp_endpoint.as_deref() {
        otlp_endpoint(raw, report);
    }
    if obs.metrics_enabled {
        metrics_access(config, report);
    }
}

/// Accepts the subset of `EnvFilter` syntax the observability crate documents:
/// comma separated `level` or `target=level`. Anything richer (span filters,
/// field matchers) is refused so that a typo cannot silently disable logging.
fn log_level(raw: &str, report: &mut Report) {
    const LEVELS: [&str; 6] = ["trace", "debug", "info", "warn", "error", "off"];
    let key = "observability.log_level";
    let mut any = false;
    for directive in raw.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        any = true;
        let (target, level) = match directive.split_once('=') {
            Some((target, level)) => (Some(target.trim()), level.trim()),
            None => (None, directive),
        };
        let level_ok = LEVELS.contains(&level.to_ascii_lowercase().as_str());
        let target_ok = target.is_none_or(|t| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':'))
        });
        if !level_ok || !target_ok {
            report.err(key, "directives must be `level` or `target=level` with level one of trace, debug, info, warn, error, off");
            return;
        }
    }
    if !any {
        report.err(key, "must not be empty");
    }
}

fn otlp_endpoint(raw: &str, report: &mut Report) {
    let key = "observability.otlp_endpoint";
    if let Some(url) = report.url(key, raw) {
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            report.err(key, "must be an http(s) URL with a host");
        }
        if url.password().is_some() {
            report.err(key, "must not contain credentials");
        }
    }
}

fn metrics_access(config: &AppConfig, report: &mut Report) {
    let obs = &config.observability;
    let key = "observability.metrics_token";
    match (obs.metrics_public, obs.metrics_token.as_ref()) {
        (true, Some(_)) => report.warn(
            key,
            "is ignored because observability.metrics_public is true",
        ),
        (true, None) => {}
        (false, None) if config.is_production() => {
            report.err(
                key,
                "is required unless observability.metrics_public is true: set RUSTRYX_OBSERVABILITY_METRICS_TOKEN or RUSTRYX_OBSERVABILITY_METRICS_TOKEN_FILE",
            );
        }
        (false, None) => report.warn(
            key,
            "not set, /metrics will be served without a token in development",
        ),
        (false, Some(token)) if token.is_blank() => report.err(key, "must not be blank"),
        (false, Some(token)) => {
            if config.is_production() && token.expose().chars().count() < 16 {
                report.err(key, "must be at least 16 characters in production");
            }
        }
    }
}
