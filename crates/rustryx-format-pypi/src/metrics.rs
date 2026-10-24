//! Metrics of the `PyPI` format, emitted through the `metrics` facade.
//!
//! The upload, cache and group counters that every format shares are emitted by
//! `rustryx-format-core`; these are the ones only this format can know. Cardinality rule
//! (SRE-09): no repository name, project name or file name is ever a label.

use metrics::counter;

/// Counter `pypi_upload_rejected_total{reason}`: an upload that was refused for a reason of the
/// protocol (`form`, `filename`, `mismatch`, `digest`, `metadata`, `exists`, `forbidden`).
pub const UPLOAD_REJECTED_TOTAL: &str = "pypi_upload_rejected_total";
/// Counter `pypi_index_requests_total{kind,source}`: an index served. `kind` is `root`,
/// `project` or `json_api`; `source` is `hosted`, `proxy` or `group`.
pub const INDEX_REQUESTS_TOTAL: &str = "pypi_index_requests_total";
/// Counter `pypi_index_too_large_total{source}`: an index that went over a size bound.
pub const INDEX_TOO_LARGE_TOTAL: &str = "pypi_index_too_large_total";
/// Counter `pypi_yank_total{op}`: `yank` or `unyank`.
pub const YANK_TOTAL: &str = "pypi_yank_total";
/// Counter `pypi_proxy_cache_evicted_total`: a cached file dropped because the upstream index
/// now lists a different digest for it.
pub const PROXY_EVICTED_TOTAL: &str = "pypi_proxy_cache_evicted_total";
/// Counter `pypi_proxy_entry_unreachable_total`: a file the upstream index lists at a URL outside
/// the configured `remote_url`, which the proxy cannot fetch.
pub const PROXY_UNREACHABLE_TOTAL: &str = "pypi_proxy_entry_unreachable_total";
/// Counter `pypi_wheel_metadata_total{result}`: reading the metadata of an uploaded wheel
/// (`ok`, `invalid`, `unavailable`).
pub const WHEEL_METADATA_TOTAL: &str = "pypi_wheel_metadata_total";

pub(crate) fn upload_rejected(reason: &'static str) {
    counter!(UPLOAD_REJECTED_TOTAL, "reason" => reason).increment(1);
}

pub(crate) fn index_served(kind: &'static str, source: &'static str) {
    counter!(INDEX_REQUESTS_TOTAL, "kind" => kind, "source" => source).increment(1);
}

pub(crate) fn index_too_large(source: &'static str) {
    counter!(INDEX_TOO_LARGE_TOTAL, "source" => source).increment(1);
}

pub(crate) fn yank(op: &'static str) {
    counter!(YANK_TOTAL, "op" => op).increment(1);
}

pub(crate) fn proxy_evicted() {
    counter!(PROXY_EVICTED_TOTAL).increment(1);
}

pub(crate) fn proxy_unreachable() {
    counter!(PROXY_UNREACHABLE_TOTAL).increment(1);
}

pub(crate) fn wheel_metadata(result: &'static str) {
    counter!(WHEEL_METADATA_TOTAL, "result" => result).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dashboards and alerts read these names: a rename here is a rename there.
    #[test]
    fn the_names_are_pinned() {
        assert_eq!(UPLOAD_REJECTED_TOTAL, "pypi_upload_rejected_total");
        assert_eq!(INDEX_REQUESTS_TOTAL, "pypi_index_requests_total");
        assert_eq!(INDEX_TOO_LARGE_TOTAL, "pypi_index_too_large_total");
        assert_eq!(YANK_TOTAL, "pypi_yank_total");
        assert_eq!(PROXY_EVICTED_TOTAL, "pypi_proxy_cache_evicted_total");
        assert_eq!(
            PROXY_UNREACHABLE_TOTAL,
            "pypi_proxy_entry_unreachable_total"
        );
        assert_eq!(WHEEL_METADATA_TOTAL, "pypi_wheel_metadata_total");
    }

    #[test]
    fn emitting_without_a_recorder_is_harmless() {
        upload_rejected("form");
        index_served("project", "hosted");
        index_too_large("proxy");
        yank("yank");
        proxy_evicted();
        proxy_unreachable();
        wheel_metadata("ok");
    }
}
