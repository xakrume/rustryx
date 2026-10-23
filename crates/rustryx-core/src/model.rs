//! Repository, component, asset and blob entities.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::audit::ActorRef;
use crate::error::DomainError;
use crate::events::EventDraft;
use crate::ids::{
    AssetId, BlobId, BlobStoreId, CleanupPolicyId, ComponentId, ProjectId, RegistryId,
    RepositoryId, RoutingRuleId, TrustedCertificateId, UserId,
};
use crate::lifecycle::SoftQuota;
use crate::names::RepoName;
use crate::project::{ProjectName, ProjectVisibility};
use crate::secret::Secret;
use crate::storage::BlobGroupConfig;

/// Package format served by a repository.
///
/// `Docker` and `Oci` are two labels over one OCI Distribution engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
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
    /// All formats, as a slice so that a format added later does not change the type. Phase 2
    /// formats of W2-G are appended here without a break.
    pub const ALL: &'static [Self] = &[
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
            .iter()
            .copied()
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
#[non_exhaustive]
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
    /// When set, `remote_url` and `auth` are resolved from this registry by the repository
    /// port, so format-core never sees the difference.
    #[serde(default)]
    pub registry_id: Option<RegistryId>,
    /// Extra CA certificates for this upstream, from the managed trust store. A certificate is
    /// public data, not a secret.
    #[serde(default)]
    pub ca_certificate_ids: Vec<TrustedCertificateId>,
    /// OCI proxies: globs on the image path; empty allows everything and the UI warns, because
    /// the upstream account defines exposure.
    #[serde(default)]
    pub namespace_allowlist: Vec<String>,
    /// Pull speed limit towards the upstream, kilobytes per second; `None` is unlimited.
    #[serde(default)]
    pub bandwidth_limit_kbps: Option<u32>,
}

/// Default freshness of cached artifacts, in seconds (one hour).
pub const DEFAULT_CONTENT_MAX_AGE_SECS: u64 = 3600;
/// Default freshness of cached metadata, in seconds (five minutes).
pub const DEFAULT_METADATA_MAX_AGE_SECS: u64 = 300;
/// Default time a 404 from upstream is remembered, in seconds.
pub const DEFAULT_NEGATIVE_CACHE_TTL_SECS: u64 = 60;
/// Default cap of concurrent upstream requests per repository.
pub const DEFAULT_MAX_CONCURRENT_UPSTREAM: u32 = 8;

impl ProxyConfig {
    /// A proxy of `remote_url` without credentials, registry, extra CA, namespace allowlist or
    /// bandwidth limit, and with the default freshness: artifacts 3600 s, metadata 300 s, a
    /// remembered 404 60 s, 8 concurrent upstream requests.
    #[must_use]
    pub fn new(remote_url: impl Into<String>) -> Self {
        Self {
            remote_url: remote_url.into(),
            auth: None,
            content_max_age_secs: DEFAULT_CONTENT_MAX_AGE_SECS,
            metadata_max_age_secs: DEFAULT_METADATA_MAX_AGE_SECS,
            negative_cache_ttl_secs: DEFAULT_NEGATIVE_CACHE_TTL_SECS,
            max_concurrent_upstream: DEFAULT_MAX_CONCURRENT_UPSTREAM,
            registry_id: None,
            ca_certificate_ids: Vec::new(),
            namespace_allowlist: Vec::new(),
            bandwidth_limit_kbps: None,
        }
    }
}

/// Settings of a group repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupConfig {
    /// Member repository names in resolution order.
    pub members: Vec<RepoName>,
}

/// Link from a repository to its project, with what authorization needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRef {
    /// The project.
    pub id: ProjectId,
    /// Its name.
    pub name: ProjectName,
    /// Its visibility.
    pub visibility: ProjectVisibility,
}

/// A repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// Whether anonymous read is allowed here (also needs the global switch). For a repository
    /// in a public project this is true and for one in a private project it is false; the
    /// database holds that invariant by trigger.
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
    /// The project the repository belongs to, if any.
    #[serde(default)]
    pub project: Option<ProjectRef>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Repository {
    /// A repository with `WritePolicy::Allow`, `allow_anonymous` false, `online` true, no
    /// quota, no policies, no routing rule, no proxy or group config, an empty
    /// `format_config`, no project and both times `now`. Assign the rest as needed.
    #[must_use]
    pub fn new(
        id: RepositoryId,
        name: RepoName,
        format: RepoFormat,
        repo_type: RepoType,
        blob_store_id: BlobStoreId,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name,
            format,
            repo_type,
            blob_store_id,
            write_policy: WritePolicy::Allow,
            allow_anonymous: false,
            online: true,
            quota_bytes: None,
            cleanup_policy_ids: Vec::new(),
            routing_rule_id: None,
            proxy: None,
            group: None,
            format_config: serde_json::json!({}),
            project: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Kind of a blob store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum BlobStoreKind {
    /// Local filesystem directory.
    Local,
    /// S3-compatible object storage.
    S3,
    /// Azure Blob Storage. Created only after the `BlobGroupsAndAzure` gate opens.
    Azure,
    /// A group of member stores. Created only after the `BlobGroupsAndAzure` gate opens.
    Group,
}

/// A configured blob store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BlobStoreDef {
    /// Identifier.
    pub id: BlobStoreId,
    /// Unique name.
    pub name: String,
    /// Local, S3, Azure or group.
    pub kind: BlobStoreKind,
    /// Kind specific settings (path, bucket, endpoint, ...). Secrets are not stored here, only
    /// references to them.
    pub config: serde_json::Value,
    /// Optional hard size limit in bytes.
    pub quota_bytes: Option<u64>,
    /// Alert only quota.
    #[serde(default)]
    pub soft_quota: Option<SoftQuota>,
    /// `Some` exactly for `kind = Group`; the members are rows of `blob_store_members`.
    #[serde(default)]
    pub group: Option<BlobGroupConfig>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl BlobStoreDef {
    /// A store without hard quota, soft quota or group configuration.
    #[must_use]
    pub fn new(
        id: BlobStoreId,
        name: impl Into<String>,
        kind: BlobStoreKind,
        config: serde_json::Value,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            config,
            quota_bytes: None,
            soft_quota: None,
            group: None,
            created_at: now,
        }
    }
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
    /// Store holding the bytes. For a group backed repository this is the concrete member
    /// store, so reads never probe members.
    pub blob_store_id: BlobStoreId,
    /// Opaque key inside the store (`UUIDv7` text).
    pub key: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Digests.
    pub checksums: Checksums,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A logical package version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// The uploader (the author a promotion approver must differ from).
    #[serde(default)]
    pub created_by: Option<UserId>,
    /// Last client download, flushed in batches. The publish time counts when it was never
    /// downloaded.
    #[serde(default, with = "crate::rfc3339::option")]
    pub last_downloaded_at: Option<OffsetDateTime>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Component {
    /// A component without namespace, version, uploader or download time.
    #[must_use]
    pub fn new(
        id: ComponentId,
        repository_id: RepositoryId,
        format: RepoFormat,
        name: impl Into<String>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            repository_id,
            format,
            group_id: None,
            name: name.into(),
            version: None,
            created_by: None,
            last_downloaded_at: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Input for creating a component (or finding the existing one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// The uploader. Not part of the coordinate: two uploads of one coordinate find one row.
    #[serde(default)]
    pub created_by: Option<UserId>,
}

impl NewComponent {
    /// A coordinate without namespace, version or uploader.
    #[must_use]
    pub fn new(repository_id: RepositoryId, format: RepoFormat, name: impl Into<String>) -> Self {
        Self {
            repository_id,
            format,
            group_id: None,
            name: name.into(),
            version: None,
            created_by: None,
        }
    }
}

/// A file addressed by `(repository, path)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
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
    #[serde(default, with = "crate::rfc3339::option")]
    pub last_downloaded_at: Option<OffsetDateTime>,
    /// The uploader id (never the client address by default).
    #[serde(default)]
    pub created_by: Option<UserId>,
    /// Creation time.
    #[serde(with = "crate::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last change time.
    #[serde(with = "crate::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Asset {
    /// An asset row from its columns, without component, download time or uploader, with both
    /// times `now`. Assign the optional fields and the real times afterwards: this is what a
    /// database row mapping uses.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "one parameter per required column of the assets row; the row mapping is the main caller"
    )]
    pub fn new(
        id: AssetId,
        repository_id: RepositoryId,
        path: impl Into<String>,
        blob_id: BlobId,
        content_type: impl Into<String>,
        size_bytes: u64,
        checksums: Checksums,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            repository_id,
            component_id: None,
            path: path.into(),
            blob_id,
            content_type: content_type.into(),
            size_bytes,
            checksums,
            last_downloaded_at: None,
            created_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// An asset that points at `blob`, taking id, size and digests from it, without component,
    /// download time or uploader.
    #[must_use]
    pub fn from_blob(
        id: AssetId,
        repository_id: RepositoryId,
        path: impl Into<String>,
        content_type: impl Into<String>,
        blob: &BlobRecord,
        now: OffsetDateTime,
    ) -> Self {
        Self::new(
            id,
            repository_id,
            path,
            blob.id,
            content_type,
            blob.size_bytes,
            blob.checksums.clone(),
            now,
        )
    }
}

/// Input for publishing an asset together with its new blob in one commit.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
    /// The uploader id.
    pub created_by: Option<UserId>,
}

impl NewAsset {
    /// A publish without component or uploader.
    #[must_use]
    pub fn new(
        repository_id: RepositoryId,
        path: impl Into<String>,
        content_type: impl Into<String>,
        blob: BlobRecord,
    ) -> Self {
        Self {
            repository_id,
            component_id: None,
            path: path.into(),
            content_type: content_type.into(),
            blob,
            created_by: None,
        }
    }
}

/// What a publish may do to an existing path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublishMode {
    /// Create or replace, subject to the repository write policy (default).
    #[default]
    Upsert,
    /// Create only. An existing path is `PolicyViolation::AlreadyExists`. Proxy cache fills
    /// use it so a fill never replaces an artifact published meanwhile, across nodes.
    CreateOnly,
}

/// Options of a state changing port call, built with [`WriteOptions::new`] and the `with_*`
/// methods. The type is `#[non_exhaustive]`, so a field added later is not a breaking change.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WriteOptions {
    /// Upsert (default) or create only.
    pub mode: PublishMode,
    /// Only an `nx-admin` driven lifecycle or admin run sets this. It sets the transaction
    /// local setting `rustryx.override_immutable`, which the SQL guards honour. Never set from
    /// request data; it is false by default and `with_override_immutable` is the only way to
    /// turn it on.
    pub override_immutable: bool,
    /// Who acts, for events and for the audit batch row. `None` means the system.
    pub actor: Option<ActorRef>,
    /// Request id for log correlation.
    pub request_id: Option<String>,
    /// Events written to the outbox in the same transaction.
    pub events: Vec<EventDraft>,
}

impl WriteOptions {
    /// Upsert, no override, no actor, no request id, no events.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the publish mode.
    #[must_use]
    pub fn with_mode(mut self, mode: PublishMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the immutability override. See the field docs: admin driven runs only.
    #[must_use]
    pub fn with_override_immutable(mut self, value: bool) -> Self {
        self.override_immutable = value;
        self
    }

    /// Sets the acting principal.
    #[must_use]
    pub fn with_actor(mut self, actor: ActorRef) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Sets the request id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Adds one outbox event.
    #[must_use]
    pub fn with_event(mut self, event: EventDraft) -> Self {
        self.events.push(event);
        self
    }

    /// Replaces the outbox events.
    #[must_use]
    pub fn with_events(mut self, events: Vec<EventDraft>) -> Self {
        self.events = events;
        self
    }
}

/// Result of publishing an asset.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PublishOutcome {
    /// The asset after the commit.
    pub asset: Asset,
    /// The blob that was replaced, to be garbage collected. `None` for a new path.
    pub replaced: Option<BlobRecord>,
    /// False when an existing path was replaced.
    pub created: bool,
}

impl PublishOutcome {
    /// Builds an outcome.
    #[must_use]
    pub fn new(asset: Asset, replaced: Option<BlobRecord>, created: bool) -> Self {
        Self {
            asset,
            replaced,
            created,
        }
    }
}

/// Publish an asset that points at an already stored blob (promotion, mount, cross repository
/// copy). Both repositories must use the same blob store, otherwise `StoreError::Conflict` and
/// the caller copies bytes.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewAssetLink {
    /// Target repository.
    pub repository_id: RepositoryId,
    /// Component, if any.
    pub component_id: Option<ComponentId>,
    /// Normalized path in the target repository.
    pub path: String,
    /// MIME type.
    pub content_type: String,
    /// The existing blob.
    pub blob_id: BlobId,
    /// The uploader id.
    pub created_by: Option<UserId>,
}

impl NewAssetLink {
    /// A link without component or uploader.
    #[must_use]
    pub fn new(
        repository_id: RepositoryId,
        path: impl Into<String>,
        content_type: impl Into<String>,
        blob_id: BlobId,
    ) -> Self {
        Self {
            repository_id,
            component_id: None,
            path: path.into(),
            content_type: content_type.into(),
            blob_id,
            created_by: None,
        }
    }
}

/// One batched download observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadTick {
    /// The downloaded asset.
    pub asset_id: AssetId,
    /// Last download time in the batch.
    pub at: OffsetDateTime,
    /// How many downloads the tick stands for.
    pub count: u32,
}

/// Who caused a read. Only `Client` increments counters and refreshes pull times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadSource {
    /// A client (a person or a build) pulled the content.
    Client,
    /// A scanner read it.
    Scanner,
    /// A replication worker read it.
    Replication,
    /// The node read it for its own purposes.
    Internal,
}

impl DownloadSource {
    /// True when the read counts as a download (counters and pull time). Scanner, replication
    /// and internal reads never do, so scanning cannot keep an old artifact alive.
    #[must_use]
    pub const fn counts_as_download(self) -> bool {
        matches!(self, Self::Client)
    }
}

#[cfg(test)]
mod tests;
