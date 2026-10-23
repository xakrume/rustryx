//! Names of the standard metrics (SRE-08) and their descriptions.
//!
//! Every crate that emits a metric uses these constants, so a dashboard or an
//! alert never depends on a typo. Labels with unbounded values are forbidden
//! (SRE-09): `route` is the matched route template, the repository name appears
//! only on `upstream_*` and `blob_store_*` series, never a path, user or address.

/// Counter `http_requests_total{route,method,status}`.
pub const HTTP_REQUESTS_TOTAL: &str = "http_requests_total";
/// Histogram `http_request_duration_seconds{route,method}`: time until the
/// response head is produced, which is the time-to-first-byte SLI for downloads.
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "http_request_duration_seconds";
/// Gauge `http_requests_in_flight`, the autoscaling signal.
pub const HTTP_REQUESTS_IN_FLIGHT: &str = "http_requests_in_flight";
/// Counter `http_shed_total{reason}`: requests refused by the service's own backpressure.
pub const HTTP_SHED_TOTAL: &str = "http_shed_total";

/// Counter `upstream_requests_total{repo,status}`.
pub const UPSTREAM_REQUESTS_TOTAL: &str = "upstream_requests_total";
/// Histogram `upstream_request_duration_seconds{repo}`.
pub const UPSTREAM_REQUEST_DURATION_SECONDS: &str = "upstream_request_duration_seconds";
/// Counter `upstream_errors_total{repo,kind}`, kind in `timeout`, `connect`, `5xx`, `circuit_open`.
pub const UPSTREAM_ERRORS_TOTAL: &str = "upstream_errors_total";

/// Gauge `db_pool_size`.
pub const DB_POOL_SIZE: &str = "db_pool_size";
/// Gauge `db_pool_idle`.
pub const DB_POOL_IDLE: &str = "db_pool_idle";
/// Gauge `db_pool_in_use`.
pub const DB_POOL_IN_USE: &str = "db_pool_in_use";
/// Histogram `db_pool_wait_seconds`.
pub const DB_POOL_WAIT_SECONDS: &str = "db_pool_wait_seconds";
/// Counter `db_pool_wait_total`: acquisitions that had to wait.
pub const DB_POOL_WAIT_TOTAL: &str = "db_pool_wait_total";

/// Counter `auth_attempts_total{method,result}`, result in ok, denied, error.
pub const AUTH_ATTEMPTS_TOTAL: &str = "auth_attempts_total";

/// Counter `background_job_runs_total{job,result}`.
pub const BACKGROUND_JOB_RUNS_TOTAL: &str = "background_job_runs_total";
/// Gauge `background_job_last_success_timestamp_seconds{job}`.
pub const BACKGROUND_JOB_LAST_SUCCESS_TIMESTAMP_SECONDS: &str =
    "background_job_last_success_timestamp_seconds";
/// Counter `distlock_lost_total{key_class}`.
pub const DISTLOCK_LOST_TOTAL: &str = "distlock_lost_total";

/// Counter `log_lines_dropped_total`: log lines discarded because the output queue was full.
pub const LOG_LINES_DROPPED_TOTAL: &str = "log_lines_dropped_total";

/// Kind of a standard metric, for [`STANDARD`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// Monotonic counter.
    Counter,
    /// Point-in-time value.
    Gauge,
    /// Distribution.
    Histogram,
}

/// Name, kind and help text of every standard metric defined so far.
pub const STANDARD: &[(&str, MetricKind, &str)] = &[
    (
        HTTP_REQUESTS_TOTAL,
        MetricKind::Counter,
        "HTTP requests by route template, method and status.",
    ),
    (
        HTTP_REQUEST_DURATION_SECONDS,
        MetricKind::Histogram,
        "Seconds until the response head was produced.",
    ),
    (
        HTTP_REQUESTS_IN_FLIGHT,
        MetricKind::Gauge,
        "Requests being processed, including body streaming.",
    ),
    (
        HTTP_SHED_TOTAL,
        MetricKind::Counter,
        "Requests refused by the service's own backpressure.",
    ),
    (
        UPSTREAM_REQUESTS_TOTAL,
        MetricKind::Counter,
        "Requests to proxy upstreams by repository and status.",
    ),
    (
        UPSTREAM_REQUEST_DURATION_SECONDS,
        MetricKind::Histogram,
        "Seconds spent on upstream requests.",
    ),
    (
        UPSTREAM_ERRORS_TOTAL,
        MetricKind::Counter,
        "Upstream errors by repository and kind.",
    ),
    (
        DB_POOL_SIZE,
        MetricKind::Gauge,
        "Connections in the database pool.",
    ),
    (
        DB_POOL_IDLE,
        MetricKind::Gauge,
        "Idle connections in the database pool.",
    ),
    (
        DB_POOL_IN_USE,
        MetricKind::Gauge,
        "Connections currently checked out.",
    ),
    (
        DB_POOL_WAIT_SECONDS,
        MetricKind::Histogram,
        "Seconds spent waiting for a pool connection.",
    ),
    (
        DB_POOL_WAIT_TOTAL,
        MetricKind::Counter,
        "Pool acquisitions that had to wait.",
    ),
    (
        AUTH_ATTEMPTS_TOTAL,
        MetricKind::Counter,
        "Authentication attempts by method and result.",
    ),
    (
        BACKGROUND_JOB_RUNS_TOTAL,
        MetricKind::Counter,
        "Background job runs by job and result.",
    ),
    (
        BACKGROUND_JOB_LAST_SUCCESS_TIMESTAMP_SECONDS,
        MetricKind::Gauge,
        "Unix time of the last successful run of each background job.",
    ),
    (
        DISTLOCK_LOST_TOTAL,
        MetricKind::Counter,
        "Distributed locks lost while held.",
    ),
    (
        LOG_LINES_DROPPED_TOTAL,
        MetricKind::Counter,
        "Log lines dropped because the output queue was full.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_follow_prometheus_conventions() {
        let mut seen = std::collections::BTreeSet::new();
        for (name, kind, help) in STANDARD {
            assert!(seen.insert(*name), "duplicate {name}");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            );
            assert_ne!(*help, "");
            if *kind == MetricKind::Counter {
                assert!(name.ends_with("_total"), "{name} is a counter");
            }
            if *kind == MetricKind::Histogram {
                assert!(name.ends_with("_seconds"), "{name} is a duration histogram");
            }
        }
    }
}
