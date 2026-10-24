//! Metrics of the maven format, through the `metrics` facade.
//!
//! Cardinality: only bounded labels (an operation, a result, a policy name). Repository, group
//! and artifact names are never labels.

/// Metadata documents handled: `op` is `generate`, `merge`, `validate_upload`, `snapshot_resolve`;
/// `result` is `ok`, `not_found`, `skipped` or an error code.
pub const METADATA_OPS_TOTAL: &str = "maven_metadata_ops_total";
/// Supplied checksum files checked against the stored file: `result` is `ok` or `mismatch`.
pub const CHECKSUM_CHECKS_TOTAL: &str = "maven_checksum_checks_total";
/// Requests refused by the format itself: `reason` is `version_policy` or `layout`.
pub const REJECTIONS_TOTAL: &str = "maven_rejections_total";

/// Counts one metadata operation.
pub(crate) fn metadata_op(op: &'static str, result: &'static str) {
    metrics::counter!(METADATA_OPS_TOTAL, "op" => op, "result" => result).increment(1);
}

/// Counts one checksum check.
pub(crate) fn checksum_check(result: &'static str) {
    metrics::counter!(CHECKSUM_CHECKS_TOTAL, "result" => result).increment(1);
}

/// Counts one refusal.
pub(crate) fn rejection(reason: &'static str) {
    metrics::counter!(REJECTIONS_TOTAL, "reason" => reason).increment(1);
}
