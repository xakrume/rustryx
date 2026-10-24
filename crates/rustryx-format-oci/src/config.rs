//! Tunables of the OCI engine and the per repository options.

use std::time::Duration;

use url::Url;

/// Largest manifest accepted or cached: 4 MiB, the size the specification asks registries
/// to support. Manifests are the only registry documents held in memory.
pub const DEFAULT_MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Limits and tunables of the engine. The defaults follow the specification and the
/// numbers of `docs/format-oci.md`.
#[derive(Debug, Clone)]
pub struct OciConfig {
    /// Largest manifest a client may push and the largest the proxy caches. Over it: 413.
    pub manifest_max_bytes: u64,
    /// Most descriptors (layers plus child manifests) one manifest may have.
    pub max_descriptors: usize,
    /// Most assets one tag listing reads while it collects the tags of an image. Beyond it
    /// the listing is refused rather than cut: a silently short tag list would make a client
    /// believe a tag does not exist.
    pub tag_scan_cap: usize,
    /// Most components one catalog request reads in total, with the same rule.
    pub catalog_scan_cap: usize,
    /// Entries of one referrers response before a `Link` to the next page.
    pub referrers_page_size: usize,
    /// Most referrers of one subject the registry reads to build the answer.
    pub referrers_scan_cap: usize,
    /// `OCI-Chunk-Min-Length` advertised when an upload starts, `None` to advertise nothing.
    /// The staged chunks of the blob store are joined at the end, so a store with a high
    /// per object cost can ask clients for bigger chunks.
    pub chunk_min_length: Option<u64>,
    /// Entries of the cache of upstream registry tokens (all proxy repositories together).
    pub upstream_token_capacity: u64,
    /// How long the result of probing an upstream's `/v2/` (open, or the bearer challenge)
    /// is remembered.
    pub upstream_probe_ttl: Duration,
    /// Longest an upstream token is cached even when the upstream says longer.
    pub upstream_token_max_ttl: Duration,
    /// Largest upstream token response read.
    pub upstream_token_response_max_bytes: u64,
}

impl Default for OciConfig {
    fn default() -> Self {
        Self {
            manifest_max_bytes: DEFAULT_MANIFEST_MAX_BYTES,
            max_descriptors: 8192,
            tag_scan_cap: 100_000,
            catalog_scan_cap: 100_000,
            referrers_page_size: 500,
            referrers_scan_cap: 5_000,
            chunk_min_length: None,
            upstream_token_capacity: 4096,
            upstream_probe_ttl: Duration::from_secs(300),
            upstream_token_max_ttl: Duration::from_secs(3600),
            upstream_token_response_max_bytes: 64 * 1024,
        }
    }
}

/// Hosts of Docker Hub's registry, where a single segment name lives in `library/`.
const DOCKER_HUB_HOSTS: [&str; 4] = [
    "registry-1.docker.io",
    "index.docker.io",
    "registry.docker.io",
    "docker.io",
];

/// Options read from a repository's `format_config` (a JSON object).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepositoryOptions {
    /// Proxy: add the `library/` namespace to a single segment image name before asking the
    /// upstream. `None` decides by the upstream host.
    pub library_namespace: Option<bool>,
}

impl RepositoryOptions {
    /// Reads the options; unknown keys and values of the wrong type are ignored (the
    /// repository editor validates them, and a bad value must not take a repository down).
    #[must_use]
    pub fn from_format_config(config: &serde_json::Value) -> Self {
        Self {
            library_namespace: config
                .get("library_namespace")
                .and_then(serde_json::Value::as_bool),
        }
    }

    /// Whether single segment names get the `library/` namespace on this upstream.
    #[must_use]
    pub fn uses_library_namespace(&self, remote: &Url) -> bool {
        self.library_namespace.unwrap_or_else(|| {
            remote
                .host_str()
                .is_some_and(|host| DOCKER_HUB_HOSTS.contains(&host.to_ascii_lowercase().as_str()))
        })
    }
}

#[cfg(test)]
mod tests;
