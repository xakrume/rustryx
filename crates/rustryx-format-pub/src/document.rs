//! The version listing (`GET api/packages/<package>`) as a typed document.
//!
//! One type serves four jobs, so the rules about what a listing contains are written once:
//! rendering a hosted package, rewriting an upstream listing for a proxy, reading a member's
//! listing for the group merge, and the single-version document of the deprecated endpoint.
//!
//! Rules that hold whatever the source:
//!
//! * **A whitelist, not a pass-through.** An entry keeps `version`, `retracted`,
//!   `archive_url`, `archive_sha256`, `pubspec` and `published` (an extension of the public
//!   registry that clients ignore). Nothing else crosses: in particular `advisoriesUpdated`
//!   is never forwarded, because phase 1 has no advisories endpoint and the client must not be
//!   told it may ask for one (and must never be given an "all clear" for data we do not have).
//! * **`archive_url` is ours.** For a proxy it is rebuilt under the repository the client
//!   addressed; the upstream's URL is read for nothing. The client attaches its credentials only
//!   to URLs below the hosted URL it was configured with, and an upstream URL would both lose
//!   them and send the client past the cache.
//! * **An entry that cannot be trusted is dropped, and counted**, never repaired: a version that
//!   is not canonical `SemVer`, a pubspec that is not an object or names another package or
//!   version, a malformed `archive_sha256`, a `retracted` that is not a boolean.
//! * **Order and uniqueness.** Versions are unique (the first occurrence wins) and sorted
//!   ascending by `SemVer` precedence. `latest` is derived from the listing itself, never copied.
//! * **`isDiscontinued` and `replacedBy` are written only when true and valid.**

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::name::PackageName;
use crate::version::PubVersion;

/// One version of a package.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionEntry {
    /// The version.
    pub version: PubVersion,
    /// True for a retracted version: still resolvable by a lock file, never chosen by default.
    pub retracted: bool,
    /// Where the archive is downloaded from, under the repository the client addressed.
    pub archive_url: String,
    /// Lowercase hex SHA-256 of the archive, when it is known.
    pub archive_sha256: Option<String>,
    /// The package manifest as a JSON object.
    pub pubspec: Map<String, Value>,
    /// Publication time as the source reported it, if it did.
    pub published: Option<String>,
}

impl VersionEntry {
    /// The entry as a member of the `versions` array.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut entry = Map::new();
        entry.insert("version".into(), Value::String(self.version.to_string()));
        entry.insert("retracted".into(), Value::Bool(self.retracted));
        entry.insert(
            "archive_url".into(),
            Value::String(self.archive_url.clone()),
        );
        if let Some(sha) = &self.archive_sha256 {
            entry.insert("archive_sha256".into(), Value::String(sha.clone()));
        }
        entry.insert("pubspec".into(), Value::Object(self.pubspec.clone()));
        if let Some(published) = &self.published {
            entry.insert("published".into(), Value::String(published.clone()));
        }
        Value::Object(entry)
    }

    /// What a hosted repository stores for a version: the entry without `archive_url`, which
    /// depends on the address the client used and is built when the document is read.
    #[must_use]
    pub fn to_stored_json(&self) -> Value {
        let mut value = self.to_json();
        if let Value::Object(map) = &mut value {
            map.remove("archive_url");
        }
        value
    }

    /// Reads an entry back from its stored form or from an upstream listing. `archive_url` is
    /// supplied by the caller. `None` means the entry is not trustworthy and is dropped.
    fn from_value(value: &Value, expected: &PackageName, archive_url: String) -> Option<Self> {
        let object = value.as_object()?;
        let version = PubVersion::parse(object.get("version")?.as_str()?).ok()?;
        let pubspec = object.get("pubspec")?.as_object()?.clone();
        // A listing for one package that carries another package's manifest, or a manifest of
        // another version, would feed the client dependencies that do not belong to this entry.
        if pubspec.get("name")?.as_str()? != expected.as_str()
            || pubspec.get("version")?.as_str()? != version.to_string()
        {
            return None;
        }
        let retracted = match object.get("retracted") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => return None,
        };
        let archive_sha256 = match object.get("archive_sha256") {
            None | Some(Value::Null) => None,
            Some(Value::String(sha)) => Some(normalize_sha256(sha)?),
            Some(_) => return None,
        };
        let published = object
            .get("published")
            .and_then(Value::as_str)
            .filter(|text| text.len() <= 64)
            .map(str::to_owned);
        Some(Self {
            version,
            retracted,
            archive_url,
            archive_sha256,
            pubspec,
            published,
        })
    }

    /// Reads a stored version document (see [`VersionEntry::to_stored_json`]).
    #[must_use]
    pub fn from_stored(value: &Value, expected: &PackageName, archive_url: String) -> Option<Self> {
        Self::from_value(value, expected, archive_url)
    }

    /// The document of the deprecated single-version endpoint.
    #[must_use]
    pub fn to_single_json(&self) -> Value {
        self.to_json()
    }
}

/// Lowercase hex of exactly 64 digits, or `None`.
fn normalize_sha256(text: &str) -> Option<String> {
    (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// The listing of one package.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageDoc {
    /// The package.
    pub name: PackageName,
    /// The package is discontinued.
    pub discontinued: bool,
    /// The package that replaces a discontinued one.
    pub replaced_by: Option<PackageName>,
    versions: Vec<VersionEntry>,
}

/// Where `archive_url` comes from when a document is read.
pub enum ArchiveUrls<'a> {
    /// The document is our own (a member's listing): keep the URL it carries.
    Keep,
    /// The document is foreign (an upstream listing): build the URL from the version.
    Rewrite(&'a dyn Fn(&PubVersion) -> String),
}

/// Why a listing could not be read at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DocError {
    /// Not JSON.
    #[error("the listing is not valid JSON")]
    NotJson,
    /// JSON, but not the shape of a listing.
    #[error("the listing is not a package document")]
    Shape,
    /// The listing is for another package.
    #[error("the listing is for another package")]
    NameMismatch,
    /// More versions than the limit.
    #[error("the listing has more versions than the limit")]
    TooManyVersions,
}

/// A read listing and how many entries had to be dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// The document.
    pub doc: PackageDoc,
    /// Entries that were not trustworthy.
    pub dropped: usize,
}

impl PackageDoc {
    /// Builds a document. Versions are sorted ascending and made unique, the first entry of a
    /// version wins.
    #[must_use]
    pub fn new(
        name: PackageName,
        discontinued: bool,
        replaced_by: Option<PackageName>,
        entries: impl IntoIterator<Item = VersionEntry>,
    ) -> Self {
        let mut unique: BTreeMap<PubVersion, VersionEntry> = BTreeMap::new();
        for entry in entries {
            unique.entry(entry.version.clone()).or_insert(entry);
        }
        Self {
            name,
            discontinued,
            replaced_by: replaced_by.filter(|_| discontinued),
            versions: unique.into_values().collect(),
        }
    }

    /// The versions, ascending.
    #[must_use]
    pub fn versions(&self) -> &[VersionEntry] {
        &self.versions
    }

    /// Looks a version up.
    #[must_use]
    pub fn find(&self, version: &PubVersion) -> Option<&VersionEntry> {
        self.versions
            .binary_search_by(|entry| entry.version.cmp(version))
            .ok()
            .and_then(|index| self.versions.get(index))
    }

    /// The version a client should prefer: the highest stable version that is not retracted,
    /// else the highest pre-release that is not retracted, else the highest of all.
    #[must_use]
    pub fn latest(&self) -> Option<&VersionEntry> {
        let newest = |keep: &dyn Fn(&VersionEntry) -> bool| {
            self.versions.iter().rev().find(|entry| keep(entry))
        };
        newest(&|e| !e.retracted && !e.version.is_prerelease())
            .or_else(|| newest(&|e| !e.retracted))
            .or_else(|| newest(&|_| true))
    }

    /// The listing as JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut root = Map::new();
        root.insert("name".into(), Value::String(self.name.to_string()));
        if self.discontinued {
            root.insert("isDiscontinued".into(), Value::Bool(true));
            if let Some(replacement) = &self.replaced_by {
                root.insert("replacedBy".into(), Value::String(replacement.to_string()));
            }
        }
        if let Some(latest) = self.latest() {
            root.insert("latest".into(), latest.to_json());
        }
        root.insert(
            "versions".into(),
            Value::Array(self.versions.iter().map(VersionEntry::to_json).collect()),
        );
        Value::Object(root)
    }

    /// The listing as response bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        // Serializing a `Value` to a `Vec` cannot fail; the empty fallback is unreachable.
        serde_json::to_vec(&self.to_json()).unwrap_or_default()
    }

    /// The document of the deprecated single-version endpoint, for one version of this listing.
    #[must_use]
    pub fn single_version_bytes(entry: &VersionEntry) -> Vec<u8> {
        serde_json::to_vec(&entry.to_single_json()).unwrap_or_default()
    }

    /// Reads a listing.
    ///
    /// # Errors
    /// A [`DocError`] when the document as a whole cannot be used. Entries that cannot be
    /// trusted are dropped and counted in [`Parsed::dropped`].
    pub fn parse(
        bytes: &[u8],
        expected: &PackageName,
        urls: &ArchiveUrls<'_>,
        max_versions: usize,
    ) -> Result<Parsed, DocError> {
        let value: Value = serde_json::from_slice(bytes).map_err(|_| DocError::NotJson)?;
        let root = value.as_object().ok_or(DocError::Shape)?;
        if root.get("name").and_then(Value::as_str) != Some(expected.as_str()) {
            return Err(DocError::NameMismatch);
        }
        let versions = root
            .get("versions")
            .and_then(Value::as_array)
            .ok_or(DocError::Shape)?;
        if versions.len() > max_versions {
            return Err(DocError::TooManyVersions);
        }
        let entries = versions
            .iter()
            .filter_map(|value| read_entry(value, expected, urls));
        let discontinued = root.get("isDiscontinued") == Some(&Value::Bool(true));
        let replaced_by = root
            .get("replacedBy")
            .and_then(Value::as_str)
            .and_then(|name| PackageName::parse(name).ok());
        let doc = Self::new(expected.clone(), discontinued, replaced_by, entries);
        // Everything that did not make it into the document: entries that could not be trusted
        // and repeats of a version that was already there.
        let dropped = versions.len().saturating_sub(doc.versions.len());
        Ok(Parsed { doc, dropped })
    }
}

fn read_entry(
    value: &Value,
    expected: &PackageName,
    urls: &ArchiveUrls<'_>,
) -> Option<VersionEntry> {
    let archive_url = match urls {
        ArchiveUrls::Keep => value.get("archive_url")?.as_str()?.to_owned(),
        ArchiveUrls::Rewrite(rewrite) => {
            let version = PubVersion::parse(value.get("version")?.as_str()?).ok()?;
            rewrite(&version)
        }
    };
    VersionEntry::from_value(value, expected, archive_url)
}

/// The success answer of the finalize step.
#[must_use]
pub fn success_json(message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "success": { "message": message } })).unwrap_or_default()
}

#[cfg(test)]
mod tests;
