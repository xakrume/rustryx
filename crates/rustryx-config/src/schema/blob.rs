//! `blob` section.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::secret::ConfigSecret;

/// Kind of the default blob store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlobKind {
    /// Directory on a local or shared volume.
    Local,
    /// S3-compatible object storage.
    S3,
}

/// Seed for the default blob store. Further stores are created through the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BlobConfig {
    /// Name under which the default store is registered.
    pub store_name: String,
    /// Local directory or S3.
    pub kind: BlobKind,
    /// Root directory of a local store.
    pub local_path: PathBuf,
    /// Percent of free space below which a local store rejects writes (0 disables).
    pub local_min_free_percent: u8,
    /// S3 endpoint URL.
    pub s3_endpoint: Option<String>,
    /// S3 bucket.
    pub s3_bucket: Option<String>,
    /// S3 region.
    pub s3_region: Option<String>,
    /// S3 access key id.
    pub s3_access_key: Option<ConfigSecret>,
    /// S3 secret access key.
    pub s3_secret_key: Option<ConfigSecret>,
    /// Use path-style addressing (`SeaweedFS`, `Ceph`).
    pub s3_path_style: bool,
    /// Multipart part size in MiB. Memory per upload is bounded by one part.
    pub s3_part_size_mb: u64,
}

impl Default for BlobConfig {
    fn default() -> Self {
        Self {
            store_name: "default".to_owned(),
            kind: BlobKind::Local,
            local_path: PathBuf::from("/var/lib/rustryx/blobs"),
            local_min_free_percent: 10,
            s3_endpoint: None,
            s3_bucket: None,
            s3_region: None,
            s3_access_key: None,
            s3_secret_key: None,
            s3_path_style: true,
            s3_part_size_mb: 16,
        }
    }
}
