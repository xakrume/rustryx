//! Metric names and the thin helpers that emit them through the `metrics` facade.
//!
//! The exporter lives in `rustryx-observability`; this crate only increments.
//! Cardinality rule (SRE-09): the repository name is allowed as a label only on
//! `upstream_*` metrics. Everything else is labelled by format, which is a closed set.
//!
//! The names that dashboards and alerts read (`docs/slo.md`,
//! `deploy/monitoring/rustryx-alerts.yaml`) are pinned by a test of this module: a rename here
//! must be a rename there.
//!
//! One metric of the SLO is NOT emitted here: `http_request_duration_seconds` is recorded by the
//! HTTP layer around [`crate::FormatEngine::serve`]. What the alerts need from this crate are its
//! two extra labels, `class` and `cache`, which the engine decides and puts on every response:
//! read them with [`RequestLabels::of`] and add them to the histogram.

use std::time::Duration;

use axum_core::body::Body;
use http::Response;
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
/// Counter `proxy_cache_total{format,result}`: `hit`, `miss`, `stale`, `negative`,
/// `revalidated` (an expired copy confirmed by the upstream or by its digest).
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
/// Counter `policy_violations_total{format,reason}`: a policy hook, a pre check or the
/// transaction of a port refused a write or a delete. `reason` is `PolicyViolation::label()`
/// (`immutable`, `held`, `quota_exceeded`, `write_policy`, `already_exists`, `read_only`,
/// `approval_required`): a bounded set, never an id or a path.
pub const POLICY_VIOLATIONS_TOTAL: &str = "policy_violations_total";
/// Gauge `download_counter_pending`: assets with downloads waiting for the next flush.
pub const DOWNLOAD_COUNTER_PENDING: &str = "download_counter_pending";
/// Counter `download_counter_flush_total{result}`: `ok` or `error`.
pub const DOWNLOAD_COUNTER_FLUSH_TOTAL: &str = "download_counter_flush_total";
/// Counter `download_counter_dropped_total`: downloads not counted because the map was full.
pub const DOWNLOAD_COUNTER_DROPPED_TOTAL: &str = "download_counter_dropped_total";
/// Histogram `http_request_duration_seconds`, recorded by the HTTP layer: its name is here so
/// that the label helper and the alerts agree on it.
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "http_request_duration_seconds";

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

pub(crate) fn policy_violation(format: RepoFormat, reason: &'static str) {
    counter!(POLICY_VIOLATIONS_TOTAL, "format" => format.as_str(), "reason" => reason).increment(1);
}

/// The kind of request, the `class` label of `http_request_duration_seconds`.
///
/// The values are the vocabulary of `docs/slo.md` (assumption A2). The alerts select two of
/// them by name: `metadata` (`RustryxMetadataLatencyP95`) and `download` with `cache="hit"`
/// (`RustryxDownloadTtfbP95`). The engine produces `Metadata`, `Download` and `Upload`;
/// `Api` and `Other` are for the HTTP layer's own routes, so that one enum is the whole
/// vocabulary and a typo cannot create a series the rules do not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RequestClass {
    /// An index or metadata document (`maven-metadata.xml`, a packument, a simple index).
    Metadata,
    /// A read of an artifact.
    Download,
    /// A write or a delete of content.
    Upload,
    /// A route of the native REST API.
    Api,
    /// Anything else, and requests that failed before they could be classified.
    #[default]
    Other,
}

impl RequestClass {
    /// The label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Download => "download",
            Self::Upload => "upload",
            Self::Api => "api",
            Self::Other => "other",
        }
    }
}

/// How the response relates to the cache, the `cache` label of `http_request_duration_seconds`.
///
/// `Hit` means "served from local storage without waiting for an upstream": a fresh or
/// revalidated proxy copy, and also a read of a hosted asset, which is the case the
/// time-to-first-byte objective is about (`docs/slo.md`: cache-miss downloads depend on the
/// upstream and are outside it). `Miss` waited for an upstream fill, `Stale` served an expired
/// copy because the upstream failed, `Bypass` was composed without a cache (a merged group
/// index), `None` is not applicable (writes, deletes, errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CacheLabel {
    /// Served from local storage without waiting for an upstream.
    Hit,
    /// Fetched from the upstream for this request.
    Miss,
    /// An expired copy, served because the upstream is unavailable.
    Stale,
    /// Composed without consulting a cache.
    Bypass,
    /// Not applicable.
    #[default]
    None,
}

impl CacheLabel {
    /// The label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Stale => "stale",
            Self::Bypass => "bypass",
            Self::None => "none",
        }
    }
}

/// The two labels the latency alerts need on `http_request_duration_seconds`.
///
/// [`crate::FormatEngine::serve`] attaches them to every response it returns (a response
/// extension: nothing goes on the wire). The HTTP layer reads them after the handler has
/// returned and records the request with them:
///
/// ```ignore
/// let labels = RequestLabels::of(&response);   // Other / None when the engine did not label it
/// histogram!(HTTP_REQUEST_DURATION_SECONDS,
///     "route" => route, "method" => method,
///     "class" => labels.class.as_str(), "cache" => labels.cache.as_str())
///     .record(elapsed.as_secs_f64());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RequestLabels {
    /// What kind of request this was.
    pub class: RequestClass,
    /// How it relates to the cache.
    pub cache: CacheLabel,
}

impl RequestLabels {
    /// Labels for a request of `class` with the given cache relation.
    #[must_use]
    pub const fn new(class: RequestClass, cache: CacheLabel) -> Self {
        Self { class, cache }
    }

    /// The labels the engine put on `response`, or the neutral `other` and `none` for a
    /// response that did not come from the engine (a 404 of the router, a health probe).
    #[must_use]
    pub fn of(response: &Response<Body>) -> Self {
        response
            .extensions()
            .get::<Self>()
            .copied()
            .unwrap_or_default()
    }

    /// Puts the labels on `response`. The HTTP layer uses it for its own routes (`Api`).
    pub fn attach(self, response: &mut Response<Body>) {
        response.extensions_mut().insert(self);
    }

    /// `[("class", ..), ("cache", ..)]`, for call sites that take label pairs.
    #[must_use]
    pub const fn pairs(self) -> [(&'static str, &'static str); 2] {
        [
            ("class", self.class.as_str()),
            ("cache", self.cache.as_str()),
        ]
    }
}
