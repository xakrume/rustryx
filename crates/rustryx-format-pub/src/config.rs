//! Tunables of the pub format.

use std::time::Duration;

use crate::archive::ArchiveLimits;
use crate::pubspec::PubspecLimits;

/// Settings of a [`crate::PubHandler`]. The defaults are the numbers the documentation of the
/// format states; a deployment changes them where it builds the handler.
#[derive(Debug, Clone)]
pub struct PubConfig {
    /// Limits of the archive reader (decompressed size, entries, ratio, paths).
    pub archive: ArchiveLimits,
    /// Limits of the manifest parser.
    pub pubspec: PubspecLimits,
    /// Largest compressed archive the upload step accepts, in bytes. It is enforced on the
    /// bytes of the file part while they stream, and the declared `Content-Length` of the whole
    /// request is checked against it before the body is read.
    pub max_upload_bytes: u64,
    /// Permits for reading archives and parsing listings on the blocking pool. Taken before the
    /// work is spawned; without one in time the client gets 503 and `Retry-After`.
    pub blocking_permits: usize,
    /// How long to wait for a permit.
    pub blocking_permit_wait: Duration,
    /// Age after which an upload that was never finalized is removed. Reaping runs when a new
    /// upload is started, so an idle repository keeps nothing but the leftovers of its last day.
    pub staging_ttl: Duration,
    /// Most versions of one package a hosted repository lists. A package past it is a server
    /// error rather than a truncated list: a truncated list would hide versions silently.
    pub max_versions_per_package: usize,
    /// Largest stored version document, in bytes.
    pub max_version_document_bytes: u64,
    /// Version documents read concurrently when a hosted package is listed.
    pub version_read_concurrency: usize,
    /// Most stale uploads removed by one reaping pass.
    pub reap_batch: usize,
}

impl Default for PubConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
        Self {
            archive: ArchiveLimits::default(),
            pubspec: PubspecLimits::default(),
            max_upload_bytes: 100 * 1024 * 1024,
            blocking_permits: cpus.saturating_mul(2),
            blocking_permit_wait: Duration::from_secs(5),
            staging_ttl: Duration::from_hours(24),
            max_versions_per_package: 10_000,
            max_version_document_bytes: 1024 * 1024,
            version_read_concurrency: 8,
            reap_batch: 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_numbers() {
        let config = PubConfig::default();
        assert_eq!(config.max_upload_bytes, 100 * 1024 * 1024);
        assert_eq!(config.archive.max_uncompressed_bytes, 256 * 1024 * 1024);
        assert_eq!(config.archive.max_ratio, 100);
        assert_eq!(config.staging_ttl, Duration::from_secs(86_400));
        assert!(config.blocking_permits >= 2);
        assert_eq!(config.blocking_permit_wait, Duration::from_secs(5));
        assert_eq!(config.pubspec.max_bytes, 256 * 1024);
    }
}
