//! The `maven-metadata.xml` document: model, strict parser, canonical writer, merge and
//! generation.
//!
//! Specification: <https://maven.apache.org/ref/current/maven-repository-metadata/repository-metadata.html>
//! (the element model) and <https://maven.apache.org/repositories/layout.html> (where the three
//! levels live).
//!
//! The parser is the trust boundary for XML that comes from a client upload, from an upstream
//! repository or from a group member. It refuses a DOCTYPE outright (so no entity can be
//! defined, which closes XXE and the billion laughs expansion at the root), resolves only the
//! five predefined entities and numeric character references, caps nesting, text length and
//! entry counts, and never panics. The writer emits one canonical form, so
//! `write(parse(write(m))) == write(m)`.

mod generate;
mod merge;
mod parse;
mod write;

pub use generate::{
    BuildAsset, VersionRecord, generate_artifact_metadata, generate_version_metadata,
};
pub use merge::merge_documents;
pub use parse::{MetadataError, parse_metadata};
pub use write::write_metadata;

/// The root element.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// `groupId`.
    pub group_id: Option<String>,
    /// `artifactId`.
    pub artifact_id: Option<String>,
    /// `version` (version level documents).
    pub version: Option<String>,
    /// `versioning`.
    pub versioning: Option<Versioning>,
    /// `plugins` (group level documents).
    pub plugins: Vec<Plugin>,
}

/// The `versioning` element.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Versioning {
    /// `latest`: the highest version, snapshots included.
    pub latest: Option<String>,
    /// `release`: the highest version that is not a snapshot.
    pub release: Option<String>,
    /// `versions/version`, in document order.
    pub versions: Vec<String>,
    /// `lastUpdated`, `yyyyMMddHHmmss` UTC.
    pub last_updated: Option<String>,
    /// `snapshot`.
    pub snapshot: Option<Snapshot>,
    /// `snapshotVersions/snapshotVersion`.
    pub snapshot_versions: Vec<SnapshotVersion>,
}

/// The `snapshot` element: the newest build of a snapshot version.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// `timestamp`, `yyyyMMdd.HHmmss` UTC.
    pub timestamp: Option<String>,
    /// `buildNumber`.
    pub build_number: Option<u32>,
}

/// One entry of `snapshotVersions`: the file `artifactId-<value>[-classifier].<extension>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotVersion {
    /// `classifier`, absent for the main artifact.
    pub classifier: Option<String>,
    /// `extension`.
    pub extension: String,
    /// `value`: the version with the timestamp, `1.0-20240102.030405-3`.
    pub value: String,
    /// `updated`, `yyyyMMddHHmmss` UTC.
    pub updated: Option<String>,
}

/// One entry of `plugins`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plugin {
    /// `name`, free text.
    pub name: Option<String>,
    /// `prefix`, the short goal prefix.
    pub prefix: String,
    /// `artifactId` of the plugin.
    pub artifact_id: String,
}

/// Most `versions/version` entries of one document.
pub const MAX_VERSIONS: usize = 100_000;
/// Most `snapshotVersion` entries of one document.
pub const MAX_SNAPSHOT_VERSIONS: usize = 10_000;
/// Most `plugin` entries of one document.
pub const MAX_PLUGINS: usize = 10_000;
