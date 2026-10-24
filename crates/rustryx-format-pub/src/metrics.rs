//! Metrics of the pub format, emitted through the `metrics` facade.
//!
//! Hosted upload, proxy and group series (`hosted_uploads_total`, `proxy_cache_total`, ...) are
//! emitted by format-core and carry `format="pub"`; this module adds what only the pub protocol
//! knows. Every label value comes from a closed set (`reason` strings of the error enums), never
//! from a package name, a version or a path, so the cardinality is fixed (SRE-09).

use metrics::counter;

/// Counter `pub_publish_total{result}`: the outcome of the finalize step. `result` is `ok`, or
/// the bounded reason of the refusal (`bomb_ratio`, `unsafe_path`, `pubspec_name`,
/// `declared_mismatch`, `version_exists`, `forbidden`, ...).
pub const PUB_PUBLISH_TOTAL: &str = "pub_publish_total";
/// Counter `pub_listing_entries_dropped_total`: entries of an upstream listing that were not
/// trustworthy (a version that is not canonical `SemVer`, a manifest that names another package,
/// a malformed digest) and were left out of what the client sees.
pub const PUB_LISTING_ENTRIES_DROPPED_TOTAL: &str = "pub_listing_entries_dropped_total";
/// Counter `pub_blocking_refused_total`: a request that found no permit for blocking work in time.
pub const PUB_BLOCKING_REFUSED_TOTAL: &str = "pub_blocking_refused_total";
/// Counter `pub_staged_uploads_reaped_total`: uploads that were never finalized and were removed.
pub const PUB_STAGED_UPLOADS_REAPED_TOTAL: &str = "pub_staged_uploads_reaped_total";
/// Counter `pub_merge_documents_skipped_total`: member listings a group merge could not read.
pub const PUB_MERGE_DOCUMENTS_SKIPPED_TOTAL: &str = "pub_merge_documents_skipped_total";

pub(crate) fn publish(result: &'static str) {
    counter!(PUB_PUBLISH_TOTAL, "result" => result).increment(1);
}

pub(crate) fn listing_entries_dropped(count: usize) {
    counter!(PUB_LISTING_ENTRIES_DROPPED_TOTAL).increment(u64::try_from(count).unwrap_or(u64::MAX));
}

pub(crate) fn blocking_refused() {
    counter!(PUB_BLOCKING_REFUSED_TOTAL).increment(1);
}

pub(crate) fn staged_reaped(count: usize) {
    counter!(PUB_STAGED_UPLOADS_REAPED_TOTAL).increment(u64::try_from(count).unwrap_or(u64::MAX));
}

pub(crate) fn merge_skipped() {
    counter!(PUB_MERGE_DOCUMENTS_SKIPPED_TOTAL).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names are read by dashboards; a rename must be deliberate.
    #[test]
    fn names_are_pinned() {
        assert_eq!(PUB_PUBLISH_TOTAL, "pub_publish_total");
        assert_eq!(
            PUB_LISTING_ENTRIES_DROPPED_TOTAL,
            "pub_listing_entries_dropped_total"
        );
        assert_eq!(PUB_BLOCKING_REFUSED_TOTAL, "pub_blocking_refused_total");
        assert_eq!(
            PUB_STAGED_UPLOADS_REAPED_TOTAL,
            "pub_staged_uploads_reaped_total"
        );
        assert_eq!(
            PUB_MERGE_DOCUMENTS_SKIPPED_TOTAL,
            "pub_merge_documents_skipped_total"
        );
    }

    #[test]
    fn emitters_do_not_need_a_recorder() {
        publish("ok");
        listing_entries_dropped(2);
        blocking_refused();
        staged_reaped(1);
        merge_skipped();
    }
}
