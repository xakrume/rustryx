//! The package document ("packument"): every version of one package, its dist-tags and times.
//!
//! Specification: <https://github.com/npm/registry/blob/main/docs/responses/package-metadata.md>
//! (full and abbreviated forms) and the document layout of the public registry API.
//!
//! The type is a validated view over a JSON document. Fields the registry does not interpret (the
//! readme, maintainers, whatever a publisher put into `package.json`) are carried in `extra` and in
//! the version manifests untouched, so a document survives a read and a write without losing data
//! this crate has never heard of.
//!
//! Two parse modes: [`ParseMode::Strict`] for documents this registry writes and for request
//! bodies (any invalid entry refuses the document), [`ParseMode::Lenient`] for documents written by
//! somebody else (an upstream registry, a member of a group): invalid versions and tags are
//! skipped, because one odd entry of a public package must not make the package unreadable.

use std::collections::BTreeMap;

use semver::Version;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::disttags::{DistTags, DistTagsError, LATEST, merge_dist_tags};
use crate::name::PackageName;
use crate::version::{parse_version, same_release};

/// Most versions one document may carry. The byte limit of the caller is the real bound; this
/// one keeps the per-version bookkeeping (a map entry each) from being the cheap way around it.
pub const MAX_VERSIONS: usize = 100_000;

/// Fields the registry copies from the latest version to the top of the document.
pub const HOISTED_FIELDS: &[&str] = &[
    "author",
    "bugs",
    "contributors",
    "description",
    "homepage",
    "keywords",
    "license",
    "maintainers",
    "readme",
    "readmeFilename",
    "repository",
];

/// Fields of a version that the abbreviated document keeps.
const ABBREVIATED_FIELDS: &[&str] = &[
    "name",
    "version",
    "deprecated",
    "dependencies",
    "acceptDependencies",
    "optionalDependencies",
    "devDependencies",
    "bundleDependencies",
    "bundledDependencies",
    "peerDependencies",
    "peerDependenciesMeta",
    "bin",
    "directories",
    "dist",
    "engines",
    "_hasShrinkwrap",
    "funding",
    "cpu",
    "os",
    "libc",
];

/// Top-level keys that are recomputed or never stored.
const DERIVED_KEYS: &[&str] = &[
    "_id",
    "_rev",
    "_attachments",
    "name",
    "dist-tags",
    "versions",
    "time",
];

/// Media type of the full document.
pub const FULL_MEDIA_TYPE: &str = "application/json";

/// Media type of the abbreviated document.
pub const ABBREVIATED_MEDIA_TYPE: &str = "application/vnd.npm.install-v1+json";

/// How strictly a document is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    /// Any invalid entry refuses the document.
    Strict,
    /// Invalid versions, tags and times are skipped.
    Lenient,
}

/// Why a document was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackumentError {
    /// Not JSON, or nested deeper than the parser allows.
    #[error("not valid JSON: {0}")]
    Json(String),
    /// The root is not an object.
    #[error("the document is not an object")]
    NotAnObject,
    /// `name` is missing or is not a string.
    #[error("the document has no name")]
    NoName,
    /// `versions` is not an object, a key is not a version, or an entry is not an object.
    #[error("invalid versions")]
    Versions,
    /// `time` is not an object of strings.
    #[error("invalid time")]
    Time,
    /// More than [`MAX_VERSIONS`] versions.
    #[error("too many versions")]
    TooManyVersions,
    /// Invalid `dist-tags`.
    #[error("invalid dist-tags: {0}")]
    Tags(#[from] DistTagsError),
}

/// A package document.
#[derive(Debug, Clone, PartialEq)]
pub struct Packument {
    name: String,
    dist_tags: DistTags,
    versions: BTreeMap<Version, Map<String, Value>>,
    time: BTreeMap<String, String>,
    extra: Map<String, Value>,
}

/// `2015-03-24T00:12:24.039Z`: UTC, millisecond precision, the form npm clients print.
#[must_use]
pub fn iso_millis(at: OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    let at = at
        .replace_nanosecond(at.nanosecond() / 1_000_000 * 1_000_000)
        .unwrap_or(at);
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
        .replace("+00:00", "Z")
}

impl Packument {
    /// An empty document for a package.
    #[must_use]
    pub fn new(name: &PackageName) -> Self {
        Self {
            name: name.full(),
            dist_tags: DistTags::new(),
            versions: BTreeMap::new(),
            time: BTreeMap::new(),
            extra: Map::new(),
        }
    }

    /// Reads a document from bytes.
    ///
    /// # Errors
    /// [`PackumentError`] for malformed JSON or, in strict mode, any invalid entry.
    pub fn from_slice(
        bytes: &[u8],
        mode: ParseMode,
        fallback_name: Option<&PackageName>,
    ) -> Result<Self, PackumentError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| PackumentError::Json(e.to_string()))?;
        Self::from_value(value, mode, fallback_name)
    }

    /// Reads a document from a JSON value, consuming it.
    ///
    /// # Errors
    /// [`PackumentError`] as for [`Packument::from_slice`].
    pub fn from_value(
        value: Value,
        mode: ParseMode,
        fallback_name: Option<&PackageName>,
    ) -> Result<Self, PackumentError> {
        let Value::Object(mut root) = value else {
            return Err(PackumentError::NotAnObject);
        };
        let name = match root.remove("name") {
            Some(Value::String(name)) if !name.is_empty() => name,
            Some(_) if mode == ParseMode::Strict => return Err(PackumentError::NoName),
            _ => match fallback_name {
                Some(name) => name.full(),
                None => return Err(PackumentError::NoName),
            },
        };
        let versions = Self::read_versions(root.remove("versions"), mode)?;
        let dist_tags = match root.remove("dist-tags") {
            None | Some(Value::Null) => DistTags::new(),
            Some(value) => match mode {
                ParseMode::Strict => DistTags::from_value_strict(&value)?,
                ParseMode::Lenient => DistTags::from_value_lenient(&value),
            },
        };
        let time = Self::read_time(root.remove("time"), mode)?;
        for key in DERIVED_KEYS {
            root.remove(*key);
        }
        Ok(Self {
            name,
            dist_tags,
            versions,
            time,
            extra: root,
        })
    }

    fn read_versions(
        value: Option<Value>,
        mode: ParseMode,
    ) -> Result<BTreeMap<Version, Map<String, Value>>, PackumentError> {
        let object = match value {
            None | Some(Value::Null) => return Ok(BTreeMap::new()),
            Some(Value::Object(object)) => object,
            Some(_) if mode == ParseMode::Lenient => return Ok(BTreeMap::new()),
            Some(_) => return Err(PackumentError::Versions),
        };
        if object.len() > MAX_VERSIONS {
            return Err(PackumentError::TooManyVersions);
        }
        let mut versions = BTreeMap::new();
        for (key, manifest) in object {
            let parsed = parse_version(&key);
            match (parsed, manifest, mode) {
                (Ok(version), Value::Object(manifest), _) => {
                    versions.insert(version, manifest);
                }
                (_, _, ParseMode::Strict) => return Err(PackumentError::Versions),
                (_, _, ParseMode::Lenient) => {}
            }
        }
        Ok(versions)
    }

    fn read_time(
        value: Option<Value>,
        mode: ParseMode,
    ) -> Result<BTreeMap<String, String>, PackumentError> {
        let object = match value {
            None | Some(Value::Null) => return Ok(BTreeMap::new()),
            Some(Value::Object(object)) => object,
            Some(_) if mode == ParseMode::Lenient => return Ok(BTreeMap::new()),
            Some(_) => return Err(PackumentError::Time),
        };
        let mut time = BTreeMap::new();
        for (key, stamp) in object {
            match (stamp, mode) {
                (Value::String(stamp), _) => {
                    time.insert(key, stamp);
                }
                (_, ParseMode::Strict) => return Err(PackumentError::Time),
                (_, ParseMode::Lenient) => {}
            }
        }
        Ok(time)
    }

    /// The package name as written in the document.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The dist-tags.
    #[must_use]
    pub fn dist_tags(&self) -> &DistTags {
        &self.dist_tags
    }

    /// The dist-tags, mutable.
    pub fn dist_tags_mut(&mut self) -> &mut DistTags {
        &mut self.dist_tags
    }

    /// The versions in `SemVer` order.
    #[must_use]
    pub fn versions(&self) -> &BTreeMap<Version, Map<String, Value>> {
        &self.versions
    }

    /// The `time` map: `created`, `modified` and one entry per version.
    #[must_use]
    pub fn time(&self) -> &BTreeMap<String, String> {
        &self.time
    }

    /// Top-level fields this crate does not interpret.
    #[must_use]
    pub fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }

    /// When the document last changed: `time.modified`, or the `modified` field of an
    /// abbreviated document.
    #[must_use]
    pub fn modified(&self) -> Option<&str> {
        self.time
            .get("modified")
            .map(String::as_str)
            .or_else(|| self.extra.get("modified").and_then(Value::as_str))
    }

    /// True when a version with the same precedence exists (`1.0.0+a` blocks `1.0.0+b`).
    #[must_use]
    pub fn has_release(&self, version: &Version) -> bool {
        self.versions.contains_key(version)
            || self
                .versions
                .keys()
                .any(|existing| same_release(existing, version))
    }

    /// The version `latest` points at, or the highest release when it points at nothing.
    #[must_use]
    pub fn latest(&self) -> Option<&Version> {
        self.dist_tags
            .get(LATEST)
            .filter(|v| self.versions.contains_key(*v))
            .or_else(|| crate::version::highest(self.versions.keys()))
    }

    /// Adds a version that is being published now, points `tags` at it, stamps the times and
    /// copies the hoisted fields from the manifest when it becomes `latest`.
    ///
    /// The caller has checked that the release does not exist yet.
    ///
    /// # Errors
    /// [`DistTagsError`] when a tag name is invalid or the tag map is full.
    pub fn add_version(
        &mut self,
        version: Version,
        manifest: Map<String, Value>,
        tags: &[String],
        now: OffsetDateTime,
    ) -> Result<(), DistTagsError> {
        let stamp = iso_millis(now);
        for tag in tags {
            self.dist_tags.set(tag, version.clone())?;
        }
        self.time
            .entry("created".to_owned())
            .or_insert_with(|| stamp.clone());
        self.time.insert(version.to_string(), stamp.clone());
        self.time.insert("modified".to_owned(), stamp);
        let becomes_latest = tags.iter().any(|t| t == LATEST);
        if becomes_latest {
            self.hoist_from(&manifest);
        }
        self.versions.insert(version, manifest);
        let keys: Vec<Version> = self.versions.keys().cloned().collect();
        self.dist_tags.repair_latest(&keys);
        Ok(())
    }

    /// Copies the hoisted fields of `manifest` to the top of the document and drops the ones the
    /// manifest does not have, so the top never shows the readme of an older version.
    pub fn hoist_from(&mut self, manifest: &Map<String, Value>) {
        for field in HOISTED_FIELDS {
            match manifest.get(*field) {
                Some(value) => {
                    self.extra.insert((*field).to_owned(), value.clone());
                }
                None => {
                    self.extra.remove(*field);
                }
            }
        }
    }

    /// Sets or clears the deprecation message of a version. An empty message clears it, which is
    /// how `npm deprecate pkg ""` undeprecates. Returns whether the version exists.
    pub fn set_deprecated(&mut self, version: &Version, message: Option<&str>) -> bool {
        let Some(manifest) = self.versions.get_mut(version) else {
            return false;
        };
        match message.filter(|m| !m.is_empty()) {
            Some(text) => {
                manifest.insert("deprecated".to_owned(), Value::String(text.to_owned()));
            }
            None => {
                manifest.remove("deprecated");
            }
        }
        true
    }

    /// Removes a version, the tags that pointed at it and its publish time. `latest` falls back
    /// to `preferred` when that version still exists, else to the highest release. The caller
    /// stamps `time.modified` ([`Packument::touch`]) so that the clock stays injectable.
    pub fn remove_version(&mut self, version: &Version, preferred: Option<&Version>) -> bool {
        let existed = self.versions.remove(version).is_some();
        self.time.remove(&version.to_string());
        let had_latest = self.dist_tags.get(LATEST) == Some(version);
        self.dist_tags.remove_pointing_at(version);
        if had_latest && let Some(choice) = preferred.filter(|p| self.versions.contains_key(*p)) {
            // The only failure of `set` is a full tag map, and a tag was just removed.
            let _ = self.dist_tags.set(LATEST, choice.clone());
        }
        let keys: Vec<Version> = self.versions.keys().cloned().collect();
        self.dist_tags.repair_latest(&keys);
        existed
    }

    /// Stamps `time.modified`.
    pub fn touch(&mut self, now: OffsetDateTime) {
        self.time.insert("modified".to_owned(), iso_millis(now));
    }

    /// Stamps `time.modified` with a text that was computed by the caller (a deterministic clock).
    pub fn set_modified(&mut self, stamp: &str) {
        self.time.insert("modified".to_owned(), stamp.to_owned());
    }

    /// Replaces the `dist.tarball` of every version with `<base>/<name>/-/<file>.tgz`.
    ///
    /// `base` is the URL of the repository the client addressed. Whatever URL the document had
    /// (an upstream's, a placeholder) is overwritten, so a client never learns a URL it cannot
    /// use and never receives one that would bypass this registry.
    pub fn rewrite_tarballs(&mut self, name: &PackageName, base: &str) {
        let base = base.trim_end_matches('/');
        for (version, manifest) in &mut self.versions {
            let url = format!("{base}/{}", name.tarball_path(version));
            let dist = manifest
                .entry("dist".to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if !dist.is_object() {
                *dist = Value::Object(Map::new());
            }
            if let Some(dist) = dist.as_object_mut() {
                dist.insert("tarball".to_owned(), Value::String(url));
            }
        }
    }

    /// The full document as JSON, with `_id` and the given `_rev`.
    #[must_use]
    pub fn to_value(&self, rev: Option<&str>) -> Value {
        let mut root = self.extra.clone();
        root.insert("_id".to_owned(), Value::String(self.name.clone()));
        if let Some(rev) = rev {
            root.insert("_rev".to_owned(), Value::String(rev.to_owned()));
        }
        root.insert("name".to_owned(), Value::String(self.name.clone()));
        root.insert("dist-tags".to_owned(), self.dist_tags.to_value());
        root.insert(
            "versions".to_owned(),
            Value::Object(
                self.versions
                    .iter()
                    .map(|(v, m)| (v.to_string(), Value::Object(m.clone())))
                    .collect(),
            ),
        );
        root.insert(
            "time".to_owned(),
            Value::Object(
                self.time
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            ),
        );
        Value::Object(root)
    }

    /// Serializes the stored form (no `_rev`: it is derived from the stored bytes).
    #[must_use]
    pub fn to_stored_vec(&self) -> Vec<u8> {
        // Serializing a `Value` cannot fail: every key is a string.
        serde_json::to_vec(&self.to_value(None)).unwrap_or_default()
    }

    /// The abbreviated document of `application/vnd.npm.install-v1+json`.
    #[must_use]
    pub fn abbreviated(&self) -> Value {
        let mut root = Map::new();
        root.insert("name".to_owned(), Value::String(self.name.clone()));
        if let Some(modified) = self.modified() {
            root.insert("modified".to_owned(), Value::String(modified.to_owned()));
        }
        root.insert("dist-tags".to_owned(), self.dist_tags.to_value());
        root.insert(
            "versions".to_owned(),
            Value::Object(
                self.versions
                    .iter()
                    .map(|(v, m)| (v.to_string(), Value::Object(abbreviate_manifest(m))))
                    .collect(),
            ),
        );
        Value::Object(root)
    }

    /// The manifest of one version, as `GET /<package>/<version>` returns it.
    #[must_use]
    pub fn version_value(&self, version: &Version) -> Option<Value> {
        self.versions
            .get(version)
            .map(|manifest| Value::Object(manifest.clone()))
    }

    /// Resolves a version, a dist-tag name or `latest` to a version of this document.
    #[must_use]
    pub fn resolve(&self, spec: &str) -> Option<Version> {
        if let Ok(version) = parse_version(spec)
            && self.versions.contains_key(&version)
        {
            return Some(version);
        }
        self.dist_tags
            .get(spec)
            .filter(|v| self.versions.contains_key(*v))
            .cloned()
    }

    /// Merges the documents of the members of a group, given in group order with a flag for the
    /// member that holds the name claim.
    ///
    /// * versions: the union; a version several members have comes from the first of them in
    ///   group order, which is the member the group's tarball lookup asks first, so a version's
    ///   `dist` and its tarball always agree;
    /// * dist-tags: a tag held by a claiming member comes from it, any other tag takes the
    ///   highest version among the members that have it;
    /// * `time`: the winner's time per version, the earliest `created`, the latest `modified`;
    /// * every other field: the claim holder's, else the first member's.
    #[must_use]
    pub fn merge(documents: &[(Self, bool)]) -> Option<Self> {
        let (first, _) = documents.first()?;
        let owner = documents
            .iter()
            .find(|(_, holds)| *holds)
            .map_or(first, |(doc, _)| doc);
        let mut merged = Self {
            name: owner.name.clone(),
            dist_tags: DistTags::new(),
            versions: BTreeMap::new(),
            time: BTreeMap::new(),
            extra: owner.extra.clone(),
        };
        let mut modified: Option<&str> = None;
        let mut created: Option<&str> = None;
        for (doc, _) in documents {
            for (version, manifest) in &doc.versions {
                if !merged.versions.contains_key(version) {
                    merged.versions.insert(version.clone(), manifest.clone());
                    if let Some(stamp) = doc.time.get(&version.to_string()) {
                        merged.time.insert(version.to_string(), stamp.clone());
                    }
                }
            }
            if let Some(m) = doc.modified() {
                modified = Some(modified.map_or(m, |current| current.max(m)));
            }
            if let Some(c) = doc.time.get("created") {
                created = Some(created.map_or(c.as_str(), |current| current.min(c.as_str())));
            }
        }
        if let Some(created) = created {
            merged.time.insert("created".to_owned(), created.to_owned());
        }
        if let Some(modified) = modified {
            merged
                .time
                .insert("modified".to_owned(), modified.to_owned());
        }
        let sets: Vec<(&DistTags, bool)> = documents
            .iter()
            .map(|(doc, holds)| (&doc.dist_tags, *holds))
            .collect();
        merged.dist_tags = merge_dist_tags(&sets);
        let keys: Vec<Version> = merged.versions.keys().cloned().collect();
        merged.dist_tags.repair_latest(&keys);
        Some(merged)
    }
}

/// The revision string of a document: `<versions>-<first 32 hex digits of its SHA-256>`.
///
/// `CouchDB` clients expect a `_rev`; this registry does not use it for concurrency control (see
/// `docs/format-npm.md`), so it is derived from the stored bytes and is the same on every node.
#[must_use]
pub fn rev_of(versions: usize, sha256_hex: &str) -> String {
    let short: String = sha256_hex.chars().take(32).collect();
    format!("{}-{short}", versions.max(1))
}

fn abbreviate_manifest(manifest: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for field in ABBREVIATED_FIELDS {
        if let Some(value) = manifest.get(*field) {
            out.insert((*field).to_owned(), value.clone());
        }
    }
    if has_install_script(manifest) {
        out.insert("hasInstallScript".to_owned(), Value::Bool(true));
    } else if let Some(flag) = manifest.get("hasInstallScript") {
        out.insert("hasInstallScript".to_owned(), flag.clone());
    }
    out
}

/// npm decides whether to run lifecycle scripts after extraction from this flag; the registry
/// computes it from the scripts of the manifest when the publisher did not send it.
fn has_install_script(manifest: &Map<String, Value>) -> bool {
    if manifest.get("hasInstallScript").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    manifest
        .get("scripts")
        .and_then(Value::as_object)
        .is_some_and(|scripts| {
            ["install", "preinstall", "postinstall"]
                .iter()
                .any(|key| scripts.contains_key(*key))
        })
}

#[cfg(test)]
mod tests;
