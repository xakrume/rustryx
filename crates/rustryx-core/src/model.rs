//! Repository, component, asset and blob entities.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DomainError;
use crate::ids::{
    AssetId, BlobId, BlobStoreId, CleanupPolicyId, ComponentId, RepositoryId, RoutingRuleId,
};
use crate::names::RepoName;
use crate::secret::Secret;

/// Package format served by a repository.
///
/// `Docker` and `Oci` are two labels over one OCI Distribution engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoFormat {
    /// Apache Maven 2 layout.
    Maven2,
    /// npm registry.
    Npm,
    /// `PyPI` simple API.
    Pypi,
    /// Docker registry label of the OCI engine.
    Docker,
    /// OCI Distribution (also Helm OCI).
    Oci,
    /// Classic Helm chart repository (`index.yaml`).
    Helm,
    /// Plain files.
    Raw,
    /// Dart and Flutter hosted pub repository (spec v2).
    Pub,
}

impl RepoFormat {
    /// All formats of phase 1.
    pub const ALL: [Self; 8] = [
        Self::Maven2,
        Self::Npm,
        Self::Pypi,
        Self::Docker,
        Self::Oci,
        Self::Helm,
        Self::Raw,
        Self::Pub,
    ];

    /// Canonical lowercase label used in URLs, JSON and the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Maven2 => "maven2",
            Self::Npm => "npm",
            Self::Pypi => "pypi",
            Self::Docker => "docker",
            Self::Oci => "oci",
            Self::Helm => "helm",
            Self::Raw => "raw",
            Self::Pub => "pub",
        }
    }
}

impl fmt::Display for RepoFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RepoFormat {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| DomainError::invalid(format!("unknown repository format '{s}'")))
    }
}

/// How a repository obtains its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoType {
    /// Content is uploaded to this repository.
    Hosted,
    /// Content is fetched from a remote repository and cached.
    Proxy,
    /// Ordered union of other repositories.
    Group,
}

impl RepoType {
    /// Canonical lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hosted => "hosted",
            Self::Proxy => "proxy",
            Self::Group => "group",
        }
    }
}

impl FromStr for RepoType {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "hosted" => Ok(Self::Hosted),
            "proxy" => Ok(Self::Proxy),
            "group" => Ok(Self::Group),
            other => Err(DomainError::invalid(format!(
                "unknown repository type '{other}'"
            ))),
        }
    }
}

/// What a hosted repository does with a second upload to an existing path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    /// Overwrite allowed.
    Allow,
    /// A path can be written once; a second write is a conflict.
    AllowOnce,
    /// Read only.
    Deny,
}

/// Credentials for an upstream (proxy) repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamAuth {
    /// Basic-auth user name.
    pub username: String,
    /// Basic-auth password. Redacted in `Debug`.
    pub password: Secret,
}

/// Settings of a proxy repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Upstream base URL. Checked against the SSRF allowlist before every use.
    pub remote_url: String,
    /// Upstream credentials, if any.
    pub auth: Option<UpstreamAuth>,
    /// Seconds a cached artifact is considered fresh.
    pub content_max_age_secs: u64,
    /// Seconds cached metadata is considered fresh.
    pub metadata_max_age_secs: u64,
    /// Seconds a 404 from upstream is remembered.
    pub negative_cache_ttl_secs: u64,
    /// Cap of concurrent upstream requests for this repository.
    pub max_concurrent_upstream: u32,
}

/// Settings of a group repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupConfig {
    /// Member repository names in resolution order.
    pub members: Vec<RepoName>,
}

/// A repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    /// Identifier.
    pub id: RepositoryId,
    /// Unique URL-safe name.
    pub name: RepoName,
    /// Package format.
    pub format: RepoFormat,
    /// Hosted, proxy or group.
    pub repo_type: RepoType,
    /// Blob store holding the content.
    pub blob_store_id: BlobStoreId,
    /// Behaviour on repeated upload.
    pub write_policy: WritePolicy,
    /// Whether anonymous read is allowed here (also needs the global switch).
    pub allow_anonymous: bool,
    /// Offline repositories answer 503 for reads.
    pub online: bool,
    /// Optional size limit in bytes (distinct blobs).
    pub quota_bytes: Option<u64>,
    /// Attached cleanup policies.
    pub cleanup_policy_ids: Vec<CleanupPolicyId>,
    /// Attached routing rule.
    pub routing_rule_id: Option<RoutingRuleId>,
    /// Present exactly for proxy repositories.
    pub proxy: Option<ProxyConfig>,
    /// Present exactly for group repositories.
    pub group: Option<GroupConfig>,
    /// Format specific settings (JSON object).
    pub format_config: serde_json::Value,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Last change time.
    pub updated_at: OffsetDateTime,
}

/// Kind of a blob store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlobStoreKind {
    /// Local filesystem directory.
    Local,
    /// S3-compatible object storage.
    S3,
}

/// A configured blob store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobStoreDef {
    /// Identifier.
    pub id: BlobStoreId,
    /// Unique name.
    pub name: String,
    /// Local or S3.
    pub kind: BlobStoreKind,
    /// Kind specific settings (path, bucket, endpoint, ...). Secrets are not stored here.
    pub config: serde_json::Value,
    /// Optional size limit in bytes.
    pub quota_bytes: Option<u64>,
    /// Creation time.
    pub created_at: OffsetDateTime,
}

/// Hex digests of one blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksums {
    /// Lowercase hex SHA-1.
    pub sha1: String,
    /// Lowercase hex SHA-256.
    pub sha256: String,
    /// Lowercase hex MD5.
    pub md5: String,
}

/// An immutable stored object. Re-publishing a path writes a new blob and
/// switches the asset row to it; the old blob is removed by GC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobRecord {
    /// Identifier.
    pub id: BlobId,
    /// Store holding the bytes.
    pub blob_store_id: BlobStoreId,
    /// Opaque key inside the store (`UUIDv7` text).
    pub key: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Digests.
    pub checksums: Checksums,
    /// Creation time.
    pub created_at: OffsetDateTime,
}

/// A logical package version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    /// Identifier.
    pub id: ComponentId,
    /// Owning repository.
    pub repository_id: RepositoryId,
    /// Format label.
    pub format: RepoFormat,
    /// Namespace (maven groupId, npm scope, docker namespace).
    pub group_id: Option<String>,
    /// Package name.
    pub name: String,
    /// Version, if the format has one.
    pub version: Option<String>,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Last change time.
    pub updated_at: OffsetDateTime,
}

/// Input for creating a component (or finding the existing one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewComponent {
    /// Owning repository.
    pub repository_id: RepositoryId,
    /// Format label.
    pub format: RepoFormat,
    /// Namespace.
    pub group_id: Option<String>,
    /// Package name.
    pub name: String,
    /// Version.
    pub version: Option<String>,
}

/// A file addressed by `(repository, path)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    /// Identifier.
    pub id: AssetId,
    /// Owning repository.
    pub repository_id: RepositoryId,
    /// Component this file belongs to, if any.
    pub component_id: Option<ComponentId>,
    /// Normalized path inside the repository, no leading slash.
    pub path: String,
    /// Current content.
    pub blob_id: BlobId,
    /// MIME type.
    pub content_type: String,
    /// Size of the current blob.
    pub size_bytes: u64,
    /// Digests of the current blob.
    pub checksums: Checksums,
    /// Last download time, flushed in batches.
    pub last_downloaded_at: Option<OffsetDateTime>,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Last change time.
    pub updated_at: OffsetDateTime,
}

/// Input for publishing an asset together with its new blob in one commit.
#[derive(Debug, Clone)]
pub struct NewAsset {
    /// Owning repository.
    pub repository_id: RepositoryId,
    /// Component, if any.
    pub component_id: Option<ComponentId>,
    /// Normalized path.
    pub path: String,
    /// MIME type.
    pub content_type: String,
    /// The already fully written blob.
    pub blob: BlobRecord,
}

/// Result of publishing an asset.
#[derive(Debug, Clone)]
pub struct PublishOutcome {
    /// The asset after the commit.
    pub asset: Asset,
    /// The blob that was replaced, to be garbage collected. `None` for a new path.
    pub replaced: Option<BlobRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_labels_round_trip() {
        for f in RepoFormat::ALL {
            assert_eq!(f.as_str().parse::<RepoFormat>().expect("parse"), f);
        }
        assert!("cargo".parse::<RepoFormat>().is_err());
    }

    #[test]
    fn format_serde_uses_lowercase_label() {
        let json = serde_json::to_string(&RepoFormat::Maven2).expect("serialize");
        assert_eq!(json, "\"maven2\"");
    }

    #[test]
    fn proxy_config_debug_hides_password() {
        let cfg = ProxyConfig {
            remote_url: "https://example.org".into(),
            auth: Some(UpstreamAuth {
                username: "u".into(),
                password: Secret::new("p4ss"),
            }),
            content_max_age_secs: 1,
            metadata_max_age_secs: 1,
            negative_cache_ttl_secs: 1,
            max_concurrent_upstream: 4,
        };
        assert!(!format!("{cfg:?}").contains("p4ss"));
    }
}
