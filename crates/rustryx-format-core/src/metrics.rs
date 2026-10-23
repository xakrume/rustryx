//! Metric names and the thin helpers that emit them through the `metrics` facade.
//!
//! The exporter lives in `rustryx-observability`; this crate only increments.
//! Cardinality rule (SRE-09): the repository name is allowed as a label only on
//! `upstream_*` metrics. Everything else is labelled by format, which is a closed set.

use std::time::Duration;

use metrics::{counter, gauge, histogram};
use rustryx_core::model::RepoFormat;

/// Counter `upstream_requests_total{repo,status}`: one per upstream attempt that got an answer
/// (`status` is the numeric code) or failed (`status` is `error`).
pub const UPSTREAM_REQUESTS_TOTAL: &str = "upstream_requests_total";
/// Histogram `upstream_request_duration_seconds{repo}`: time to response headers.
pub const UPSTREAM_REQUEST_DURATION_SECONDS: &str = "upstream_request_duration_seconds";
/// Counter `upstream_errors_total{repo,kind}`.
pub const UPSTREAM_ERRORS_TOTAL: &str = "upstream_errors_total";
/// Gauge `upstream_circuit_state{repo}`: 0 closed, 1 half-open, 2 open.
pub const UPSTREAM_CIRCUIT_STATE: &str = "upstream_circuit_state";
/// Counter `proxy_cache_total{format,result}`: `hit`, `miss`, `stale`, `negative`.
pub const PROXY_CACHE_TOTAL: &str = "proxy_cache_total";
/// Counter `singleflight_total{role}`: `leader` or `follower`. Followers are the waiters.
pub const SINGLEFLIGHT_TOTAL: &str = "singleflight_total";
/// Counter `hosted_uploads_total{format,result}`.
pub const HOSTED_UPLOADS_TOTAL: &str = "hosted_uploads_total";
/// Counter `hosted_upload_bytes_total{format}`: bytes of successfully published uploads.
pub const HOSTED_UPLOAD_BYTES_TOTAL: &str = "hosted_upload_bytes_total";
/// Counter `group_member_skipped_total{reason}`: `denied`, `claimed`, `failed`, `routing`, `cycle`.
pub const GROUP_MEMBER_SKIPPED_TOTAL: &str = "group_member_skipped_total";
/// Counter `blob_orphan_leak_total`: a committed blob could not be deleted after a failed publish.
pub const BLOB_ORPHAN_LEAK_TOTAL: &str = "blob_orphan_leak_total";
/// Counter `blob_writer_abandoned_total`: a writer was dropped without commit or explicit abort.
pub const BLOB_WRITER_ABANDONED_TOTAL: &str = "blob_writer_abandoned_total";
/// Counter `hosted_allow_once_violation_total`: an `allow_once` path was overwritten by a
/// cross-node race that the in-process reservation could not see.
pub const ALLOW_ONCE_VIOLATION_TOTAL: &str = "hosted_allow_once_violation_total";
/// Counter `proxy_client_detached_total`: the tee client stalled or left, the fill went on alone.
pub const PROXY_CLIENT_DETACHED_TOTAL: &str = "proxy_client_detached_total";

pub(crate) fn upstream_request(repo: &str, status: &str, elapsed: Duration) {
    counter!(UPSTREAM_REQUESTS_TOTAL, "repo" => repo.to_owned(), "status" => status.to_owned())
        .increment(1);
    histogram!(UPSTREAM_REQUEST_DURATION_SECONDS, "repo" => repo.to_owned())
        .record(elapsed.as_secs_f64());
}

pub(crate) fn upstream_error(repo: &str, kind: &'static str) {
    counter!(UPSTREAM_ERRORS_TOTAL, "repo" => repo.to_owned(), "kind" => kind).increment(1);
}

pub(crate) fn circuit_state(repo: &str, state: u8) {
    gauge!(UPSTREAM_CIRCUIT_STATE, "repo" => repo.to_owned()).set(f64::from(state));
}

pub(crate) fn cache_result(format: RepoFormat, result: &'static str) {
    counter!(PROXY_CACHE_TOTAL, "format" => format.as_str(), "result" => result).increment(1);
}

pub(crate) fn singleflight(role: &'static str) {
    counter!(SINGLEFLIGHT_TOTAL, "role" => role).increment(1);
}

pub(crate) fn upload_result(format: RepoFormat, result: &'static str) {
    counter!(HOSTED_UPLOADS_TOTAL, "format" => format.as_str(), "result" => result).increment(1);
}

pub(crate) fn upload_bytes(format: RepoFormat, bytes: u64) {
    counter!(HOSTED_UPLOAD_BYTES_TOTAL, "format" => format.as_str()).increment(bytes);
}

pub(crate) fn group_member_skipped(reason: &'static str) {
    counter!(GROUP_MEMBER_SKIPPED_TOTAL, "reason" => reason).increment(1);
}

pub(crate) fn count(name: &'static str) {
    counter!(name).increment(1);
}
