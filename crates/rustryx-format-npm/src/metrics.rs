//! Metrics of the npm format, through the `metrics` facade.
//!
//! Names are unprefixed like the rest of the registry's series (see `docs/slo.md`). Label values
//! are bounded sets, never package names or versions.

/// Publishes, by outcome: `ok` or the stable error code.
pub const PUBLISH_TOTAL: &str = "npm_publish_total";
/// Decoded tarball bytes accepted by publishes.
pub const PUBLISH_BYTES_TOTAL: &str = "npm_publish_bytes_total";
/// Rounds of a package document change lost to a concurrent writer (create-only conflict).
pub const DOCUMENT_CAS_RETRY_TOTAL: &str = "npm_document_cas_retry_total";
/// Dist-tag changes by `op`: `set` or `remove`.
pub const DIST_TAG_CHANGES_TOTAL: &str = "npm_dist_tag_changes_total";
/// Unpublishes by `scope`: `version` or `package`.
pub const UNPUBLISH_TOTAL: &str = "npm_unpublish_total";
/// Package documents served, by `source` (`hosted`, `proxy`, `group`) and `variant` (`full`,
/// `abbreviated`).
pub const PACKUMENT_SERVED_TOTAL: &str = "npm_packument_served_total";
/// Group merges of package documents, by `result`: `ok` or `skipped_member`.
pub const GROUP_MERGE_TOTAL: &str = "npm_group_merge_total";
/// Proxied tarballs refused because the upstream packument has no digest for them or the
/// version is not listed, by `reason`.
pub const PROXY_TARBALL_REFUSED_TOTAL: &str = "npm_proxy_tarball_refused_total";
/// Logins and logouts through the legacy endpoints, by `op` and `result`.
pub const SESSION_TOTAL: &str = "npm_session_total";

pub(crate) fn publish_result(result: &str) {
    metrics::counter!(PUBLISH_TOTAL, "result" => result.to_owned()).increment(1);
}

pub(crate) fn publish_bytes(bytes: u64) {
    metrics::counter!(PUBLISH_BYTES_TOTAL).increment(bytes);
}

pub(crate) fn cas_retry() {
    metrics::counter!(DOCUMENT_CAS_RETRY_TOTAL).increment(1);
}

pub(crate) fn dist_tag_change(op: &'static str) {
    metrics::counter!(DIST_TAG_CHANGES_TOTAL, "op" => op).increment(1);
}

pub(crate) fn unpublish(scope: &'static str) {
    metrics::counter!(UNPUBLISH_TOTAL, "scope" => scope).increment(1);
}

pub(crate) fn packument_served(source: &'static str, variant: &'static str) {
    metrics::counter!(PACKUMENT_SERVED_TOTAL, "source" => source, "variant" => variant)
        .increment(1);
}

pub(crate) fn group_merge(result: &'static str) {
    metrics::counter!(GROUP_MERGE_TOTAL, "result" => result).increment(1);
}

pub(crate) fn proxy_tarball_refused(reason: &'static str) {
    metrics::counter!(PROXY_TARBALL_REFUSED_TOTAL, "reason" => reason).increment(1);
}

pub(crate) fn session(op: &'static str, result: &'static str) {
    metrics::counter!(SESSION_TOTAL, "op" => op, "result" => result).increment(1);
}
