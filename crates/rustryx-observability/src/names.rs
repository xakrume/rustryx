//! Names of the standard metrics (SRE-08) and their descriptions.
//!
//! Every crate that emits a metric uses these constants, so a dashboard or an
//! alert never depends on a typo. Labels with unbounded values are forbidden
//! (SRE-09): `route` is the matched route template, the repository name appears
//! only on `upstream_*` and `blob_store_*` series, never a path, user or address.

/// Counter `http_requests_total{route,method,status}`.
pub const HTTP_REQUESTS_TOTAL: &str = "http_requests_total";
/// Histogram `http_request_duration_seconds{route,method,class,cache}`: time until the
/// response head is produced, which is the time-to-first-byte SLI for downloads. `class`
/// is `metadata`, `download`, `upload`, `api` or `other`; `cache` is `hit`, `miss` or `none`
/// (see [`crate::RequestLabels`]).
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
/// Gauge `upstream_circuit_state{repo}`: 0 closed, 1 half-open, 2 open.
pub const UPSTREAM_CIRCUIT_STATE: &str = "upstream_circuit_state";
/// Counter `proxy_cache_total{format,result}`: `hit`, `miss`, `stale`, `negative`, `revalidated`.
pub const PROXY_CACHE_TOTAL: &str = "proxy_cache_total";
/// Counter `singleflight_total{role}`: `leader` or `follower` of a proxy cache fill.
pub const SINGLEFLIGHT_TOTAL: &str = "singleflight_total";
/// Counter `hosted_uploads_total{format,result}`.
pub const HOSTED_UPLOADS_TOTAL: &str = "hosted_uploads_total";
/// Counter `hosted_upload_bytes_total{format}`: bytes of successfully published uploads.
pub const HOSTED_UPLOAD_BYTES_TOTAL: &str = "hosted_upload_bytes_total";
/// Counter `group_member_skipped_total{reason}`: `denied`, `claimed`, `failed`, `routing`, `cycle`.
pub const GROUP_MEMBER_SKIPPED_TOTAL: &str = "group_member_skipped_total";
/// Counter `blob_orphan_leak_total`: a committed blob could not be deleted after a failed publish.
pub const BLOB_ORPHAN_LEAK_TOTAL: &str = "blob_orphan_leak_total";
/// Counter `blob_writer_abandoned_total`: a writer was dropped without commit or abort.
pub const BLOB_WRITER_ABANDONED_TOTAL: &str = "blob_writer_abandoned_total";
/// Counter `hosted_allow_once_violation_total`: an `allow_once` path was overwritten by a race
/// between nodes. A tripwire: it must stay at zero.
pub const HOSTED_ALLOW_ONCE_VIOLATION_TOTAL: &str = "hosted_allow_once_violation_total";
/// Counter `proxy_client_detached_total`: the client of a cache fill stalled or left and the
/// fill went on alone.
pub const PROXY_CLIENT_DETACHED_TOTAL: &str = "proxy_client_detached_total";
/// Counter `policy_violations_total{format,reason}`: a write or delete refused by policy.
pub const POLICY_VIOLATIONS_TOTAL: &str = "policy_violations_total";
/// Gauge `download_counter_pending`: assets with downloads waiting for the next flush.
pub const DOWNLOAD_COUNTER_PENDING: &str = "download_counter_pending";
/// Counter `download_counter_flush_total{result}`: `ok` or `error`.
pub const DOWNLOAD_COUNTER_FLUSH_TOTAL: &str = "download_counter_flush_total";
/// Counter `download_counter_dropped_total`: downloads not counted because the map was full.
pub const DOWNLOAD_COUNTER_DROPPED_TOTAL: &str = "download_counter_dropped_total";

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

/// Counter `auth_breakglass_logins_total`: sign-ins of the local break-glass administrator.
/// Emitted by `rustryx-auth` through the `metrics` facade (see `AuthServiceConfig`).
pub const AUTH_BREAKGLASS_LOGINS_TOTAL: &str = "auth_breakglass_logins_total";

/// Counter `background_job_runs_total{task,result}`, result in `ok`, `error`, `skipped`
/// (a supervised daemon that panics or exits counts as `panic` or `exit`). The label is
/// `task`, not `job`: a series label named `job` collides with the scrape label, and
/// Prometheus renames it to `exported_job` on ingestion.
pub const BACKGROUND_JOB_RUNS_TOTAL: &str = "background_job_runs_total";
/// Gauge `background_job_last_success_timestamp_seconds{task}`, initialised to the process
/// start time when a task is registered, so that a task that never ran goes stale.
pub const BACKGROUND_JOB_LAST_SUCCESS_TIMESTAMP_SECONDS: &str =
    "background_job_last_success_timestamp_seconds";
/// Gauge `background_job_running{task}`: 1 while a run is in progress.
pub const BACKGROUND_JOB_RUNNING: &str = "background_job_running";
/// Counter `distlock_lost_total{key_class}`.
pub const DISTLOCK_LOST_TOTAL: &str = "distlock_lost_total";

/// Gauge `db_up`: 1 while the last readiness `SELECT 1` succeeded, 0 otherwise.
pub const DB_UP: &str = "db_up";
/// Gauge `redis_up`: 1 while the last Redis check succeeded, 0 otherwise. Absent while no
/// Redis is configured.
pub const REDIS_UP: &str = "redis_up";

/// Gauge `tokio_busy_ratio`: share of worker time spent running tasks since the last sample
/// (0 to 1, averaged over the workers).
pub const TOKIO_BUSY_RATIO: &str = "tokio_busy_ratio";
/// Gauge `tokio_global_queue_depth`: tasks waiting in the runtime's global queue.
pub const TOKIO_GLOBAL_QUEUE_DEPTH: &str = "tokio_global_queue_depth";
/// Gauge `tokio_workers`: worker threads of the runtime.
pub const TOKIO_WORKERS: &str = "tokio_workers";
/// Gauge `tokio_alive_tasks`: tasks spawned and not yet finished.
pub const TOKIO_ALIVE_TASKS: &str = "tokio_alive_tasks";

/// Gauge `process_cpu_seconds_total`: user plus system CPU time of the process. A gauge
/// because the `metrics` counter is an integer and whole seconds are too coarse for a rate
/// over a few minutes.
pub const PROCESS_CPU_SECONDS_TOTAL: &str = "process_cpu_seconds_total";
/// Gauge `process_resident_memory_bytes`.
pub const PROCESS_RESIDENT_MEMORY_BYTES: &str = "process_resident_memory_bytes";
/// Gauge `process_virtual_memory_bytes`.
pub const PROCESS_VIRTUAL_MEMORY_BYTES: &str = "process_virtual_memory_bytes";
/// Gauge `process_open_fds`.
pub const PROCESS_OPEN_FDS: &str = "process_open_fds";
/// Gauge `process_max_fds`: the soft limit of open files.
pub const PROCESS_MAX_FDS: &str = "process_max_fds";
/// Gauge `process_start_time_seconds`: Unix time the process started.
pub const PROCESS_START_TIME_SECONDS: &str = "process_start_time_seconds";

/// Gauge `oidc_jwks_age_seconds`: age of the cached Keycloak key set. Emitted by `rustryx-auth`
/// while `OidcProvider::spawn_background` runs.
pub const OIDC_JWKS_AGE_SECONDS: &str = "oidc_jwks_age_seconds";
/// Counter `oidc_requests_total{op,result}`: calls to Keycloak, op in `discovery`, `jwks`,
/// result in `ok`, `error`, `timeout`. Emitted by `rustryx-auth`.
pub const OIDC_REQUESTS_TOTAL: &str = "oidc_requests_total";
/// Counter `ldap_requests_total{result}`: LDAP operations, result in `ok`, `error`, `timeout`.
/// Emitted by `rustryx-auth`.
pub const LDAP_REQUESTS_TOTAL: &str = "ldap_requests_total";

/// Counter `rbac_decisions_total{kind,result}`: authorization questions answered, kind in
/// `content`, `app`, `project`, `admin`, result in `allow`, `deny`, `error`. Emitted by
/// `rustryx-rbac`.
pub const RBAC_DECISIONS_TOTAL: &str = "rbac_decisions_total";
/// Counter `rbac_cache_events_total{cache,event}`: cache calls of the authorizer, cache in
/// `privileges`, `builtin_roles`, `selectors`, `programs`, event in `hit`, `miss`, `error`.
pub const RBAC_CACHE_EVENTS_TOTAL: &str = "rbac_cache_events_total";
/// Counter `rbac_invalidations_total{event}`: invalidation messages applied on this node.
pub const RBAC_INVALIDATIONS_TOTAL: &str = "rbac_invalidations_total";

/// Counter `blob_store_ops_total{store,op,result}`. Emitted by `rustryx-blob`.
pub const BLOB_STORE_OPS_TOTAL: &str = "blob_store_ops_total";
/// Counter `blob_store_bytes_total{store,direction}`. Emitted by `rustryx-blob`.
pub const BLOB_STORE_BYTES_TOTAL: &str = "blob_store_bytes_total";
/// Gauge `blob_store_free_bytes{store}`: bytes a non-root process can still write on a local
/// store.
pub const BLOB_STORE_FREE_BYTES: &str = "blob_store_free_bytes";
/// Gauge `blob_store_total_bytes{store}`: size of the file system under a local store.
pub const BLOB_STORE_TOTAL_BYTES: &str = "blob_store_total_bytes";
/// Counter `distlock_acquire_total{key_class,result}`, result in `acquired`, `busy`, `error`.
pub const DISTLOCK_ACQUIRE_TOTAL: &str = "distlock_acquire_total";

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
        UPSTREAM_CIRCUIT_STATE,
        MetricKind::Gauge,
        "Circuit breaker state per upstream repository: 0 closed, 1 half-open, 2 open.",
    ),
    (
        PROXY_CACHE_TOTAL,
        MetricKind::Counter,
        "Proxy cache lookups by format and result.",
    ),
    (
        SINGLEFLIGHT_TOTAL,
        MetricKind::Counter,
        "Proxy cache fills by role: the leader fetches, followers wait.",
    ),
    (
        HOSTED_UPLOADS_TOTAL,
        MetricKind::Counter,
        "Uploads to hosted repositories by format and result.",
    ),
    (
        HOSTED_UPLOAD_BYTES_TOTAL,
        MetricKind::Counter,
        "Bytes of successfully published uploads by format.",
    ),
    (
        GROUP_MEMBER_SKIPPED_TOTAL,
        MetricKind::Counter,
        "Group members skipped while resolving a request, by reason.",
    ),
    (
        BLOB_ORPHAN_LEAK_TOTAL,
        MetricKind::Counter,
        "Committed blobs that could not be deleted after a failed publish.",
    ),
    (
        BLOB_WRITER_ABANDONED_TOTAL,
        MetricKind::Counter,
        "Blob writers dropped without commit or abort.",
    ),
    (
        HOSTED_ALLOW_ONCE_VIOLATION_TOTAL,
        MetricKind::Counter,
        "Paths published once that were overwritten by a race between nodes. Must stay zero.",
    ),
    (
        PROXY_CLIENT_DETACHED_TOTAL,
        MetricKind::Counter,
        "Proxy cache fills that went on after their client stalled or left.",
    ),
    (
        POLICY_VIOLATIONS_TOTAL,
        MetricKind::Counter,
        "Writes and deletes refused by policy, by format and reason.",
    ),
    (
        DOWNLOAD_COUNTER_PENDING,
        MetricKind::Gauge,
        "Assets with downloads waiting for the next counter flush.",
    ),
    (
        DOWNLOAD_COUNTER_FLUSH_TOTAL,
        MetricKind::Counter,
        "Download counter flushes by result.",
    ),
    (
        DOWNLOAD_COUNTER_DROPPED_TOTAL,
        MetricKind::Counter,
        "Downloads not counted because the pending map was full.",
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
        AUTH_BREAKGLASS_LOGINS_TOTAL,
        MetricKind::Counter,
        "Sign-ins of the local break-glass administrator.",
    ),
    (
        BACKGROUND_JOB_RUNS_TOTAL,
        MetricKind::Counter,
        "Background task runs by task and result.",
    ),
    (
        BACKGROUND_JOB_LAST_SUCCESS_TIMESTAMP_SECONDS,
        MetricKind::Gauge,
        "Unix time of the last successful run of each background task.",
    ),
    (
        BACKGROUND_JOB_RUNNING,
        MetricKind::Gauge,
        "1 while a run of the background task is in progress.",
    ),
    (
        DISTLOCK_LOST_TOTAL,
        MetricKind::Counter,
        "Distributed locks lost while held.",
    ),
    (
        DB_UP,
        MetricKind::Gauge,
        "1 while the last PostgreSQL readiness query succeeded.",
    ),
    (
        REDIS_UP,
        MetricKind::Gauge,
        "1 while the last Redis check succeeded.",
    ),
    (
        TOKIO_BUSY_RATIO,
        MetricKind::Gauge,
        "Share of tokio worker time spent running tasks since the last sample.",
    ),
    (
        TOKIO_GLOBAL_QUEUE_DEPTH,
        MetricKind::Gauge,
        "Tasks waiting in the tokio global queue.",
    ),
    (TOKIO_WORKERS, MetricKind::Gauge, "Tokio worker threads."),
    (
        TOKIO_ALIVE_TASKS,
        MetricKind::Gauge,
        "Tokio tasks spawned and not yet finished.",
    ),
    (
        PROCESS_CPU_SECONDS_TOTAL,
        MetricKind::Gauge,
        "User plus system CPU seconds used by the process.",
    ),
    (
        PROCESS_RESIDENT_MEMORY_BYTES,
        MetricKind::Gauge,
        "Resident memory of the process in bytes.",
    ),
    (
        PROCESS_VIRTUAL_MEMORY_BYTES,
        MetricKind::Gauge,
        "Virtual memory of the process in bytes.",
    ),
    (
        PROCESS_OPEN_FDS,
        MetricKind::Gauge,
        "Open file descriptors of the process.",
    ),
    (
        PROCESS_MAX_FDS,
        MetricKind::Gauge,
        "Soft limit of open file descriptors.",
    ),
    (
        PROCESS_START_TIME_SECONDS,
        MetricKind::Gauge,
        "Unix time the process started.",
    ),
    (
        OIDC_JWKS_AGE_SECONDS,
        MetricKind::Gauge,
        "Age of the cached Keycloak key set in seconds.",
    ),
    (
        OIDC_REQUESTS_TOTAL,
        MetricKind::Counter,
        "Calls to Keycloak by operation and result.",
    ),
    (
        LDAP_REQUESTS_TOTAL,
        MetricKind::Counter,
        "LDAP operations by result.",
    ),
    (
        DISTLOCK_ACQUIRE_TOTAL,
        MetricKind::Counter,
        "Distributed lock acquisitions by key class and result.",
    ),
    (
        RBAC_DECISIONS_TOTAL,
        MetricKind::Counter,
        "Authorization questions answered by kind and result.",
    ),
    (
        RBAC_CACHE_EVENTS_TOTAL,
        MetricKind::Counter,
        "Authorizer cache calls by cache and event.",
    ),
    (
        RBAC_INVALIDATIONS_TOTAL,
        MetricKind::Counter,
        "Invalidation messages applied by the authorizer.",
    ),
    (
        BLOB_STORE_OPS_TOTAL,
        MetricKind::Counter,
        "Blob store operations by store, operation and result.",
    ),
    (
        BLOB_STORE_BYTES_TOTAL,
        MetricKind::Counter,
        "Bytes moved through a blob store by direction.",
    ),
    (
        BLOB_STORE_FREE_BYTES,
        MetricKind::Gauge,
        "Free bytes under a local blob store.",
    ),
    (
        BLOB_STORE_TOTAL_BYTES,
        MetricKind::Gauge,
        "Size in bytes of the file system under a local blob store.",
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
