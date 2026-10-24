//! The in-memory model of simple repository pages, shared by the hosted generator, the proxy
//! rewriter and the group merger.
//!
//! Whatever representation a page arrives in (HTML or JSON, from a hosted repository, an
//! upstream or a group member), it is parsed into this model and rendered again for the client
//! in the representation the client asked for. That is what lets a proxy serve JSON from an
//! upstream that only speaks HTML and a group merge members that speak different dialects.

use std::collections::BTreeMap;

use crate::filename::DistFilename;
use crate::version::Version;

/// Most files in one project page, generated or parsed. A page over it is refused: it bounds the
/// memory of a parse and the size of a generated document.
pub const MAX_FILES_PER_PAGE: usize = 50_000;
/// Most projects in one root index.
pub const MAX_PROJECTS_PER_INDEX: usize = 1_000_000;

/// The hash of the core metadata file, or the bare statement that there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreMetadataRef {
    /// `data-core-metadata="true"`: the file exists, its hash is not given.
    Available,
    /// The hashes of the metadata file (algorithm to lowercase hex).
    Hashes(BTreeMap<String, String>),
}

/// One distribution file of a project page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// The file name.
    pub filename: String,
    /// Absolute URL of the file, without the hash fragment.
    pub url: String,
    /// Hashes of the file (algorithm to lowercase hex).
    pub hashes: BTreeMap<String, String>,
    /// `Requires-Python` of the file.
    pub requires_python: Option<String>,
    /// `Some` when the file is yanked (PEP 592); the text is the reason, empty when none was given.
    pub yanked: Option<String>,
    /// The core metadata file of the distribution (PEP 658 and 714).
    pub core_metadata: Option<CoreMetadataRef>,
    /// Size in bytes.
    pub size: Option<u64>,
    /// Upload time, ISO 8601 UTC with a `Z` suffix (PEP 700).
    pub upload_time: Option<String>,
}

impl FileEntry {
    /// A bare entry: file name and URL.
    #[must_use]
    pub fn new(filename: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            url: url.into(),
            hashes: BTreeMap::new(),
            requires_python: None,
            yanked: None,
            core_metadata: None,
            size: None,
            upload_time: None,
        }
    }

    /// The canonical version of the file, from its name.
    #[must_use]
    pub fn version(&self) -> Option<Version> {
        DistFilename::parse(&self.filename)
            .ok()
            .map(|parsed| parsed.version().clone())
    }

    /// The hash that goes into the URL fragment: SHA-256 when there is one, else the first.
    #[must_use]
    pub fn fragment_hash(&self) -> Option<(&str, &str)> {
        preferred_hash(&self.hashes)
    }
}

/// SHA-256 when present, otherwise the first entry in algorithm order.
pub(crate) fn preferred_hash(hashes: &BTreeMap<String, String>) -> Option<(&str, &str)> {
    hashes
        .get_key_value("sha256")
        .or_else(|| hashes.iter().next())
        .map(|(algo, value)| (algo.as_str(), value.as_str()))
}

/// A project page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPage {
    /// The normalized project name. It is the name the request was for, never one read from the
    /// document: a document cannot claim to be another project.
    pub name: String,
    /// The files.
    pub files: Vec<FileEntry>,
}

impl ProjectPage {
    /// The canonical versions of the files, each once, in PEP 440 order.
    #[must_use]
    pub fn versions(&self) -> Vec<String> {
        let mut parsed: Vec<Version> = self.files.iter().filter_map(FileEntry::version).collect();
        parsed.sort();
        parsed.dedup();
        parsed.iter().map(ToString::to_string).collect()
    }

    /// Sorts the files by version, then by name, so that a generated page is stable.
    pub fn sort_files(&mut self) {
        self.files.sort_by(|a, b| {
            a.version()
                .cmp(&b.version())
                .then_with(|| a.filename.cmp(&b.filename))
        });
    }
}

/// The root index: the names of the projects, normalized, sorted, each once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RootPage {
    /// The project names.
    pub projects: Vec<String>,
}

impl RootPage {
    /// Builds a root page from names, sorting and removing duplicates.
    #[must_use]
    pub fn from_names(mut names: Vec<String>) -> Self {
        names.sort();
        names.dedup();
        Self { projects: names }
    }
}

/// Why a document could not be parsed into a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The document is not valid UTF-8, or not valid JSON.
    #[error("the index document cannot be parsed")]
    Malformed,
    /// More files or projects than the bounds allow.
    #[error("the index document has too many entries")]
    TooMany,
}

/// Lowercase hex check used for every hash read from a document.
pub(crate) fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_unique_and_in_pep_440_order() {
        let page = ProjectPage {
            name: "foo".into(),
            files: [
                "foo-1.10.tar.gz",
                "foo-1.2-py3-none-any.whl",
                "foo-1.2.tar.gz",
                "foo-1.0rc1.tar.gz",
                "bad.egg",
            ]
            .into_iter()
            .map(|f| FileEntry::new(f, "https://x/y"))
            .collect(),
        };
        assert_eq!(page.versions(), ["1.0rc1", "1.2", "1.10"]);
    }

    #[test]
    fn sorting_is_stable_by_version_then_name() {
        let mut page = ProjectPage {
            name: "foo".into(),
            files: [
                "foo-2.0.tar.gz",
                "foo-1.0-py3-none-any.whl",
                "foo-1.0.tar.gz",
            ]
            .into_iter()
            .map(|f| FileEntry::new(f, "u"))
            .collect(),
        };
        page.sort_files();
        let names: Vec<_> = page.files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(
            names,
            [
                "foo-1.0-py3-none-any.whl",
                "foo-1.0.tar.gz",
                "foo-2.0.tar.gz"
            ]
        );
    }

    #[test]
    fn fragment_hash_prefers_sha256() {
        let mut entry = FileEntry::new("a-1.tar.gz", "u");
        assert_eq!(entry.fragment_hash(), None);
        entry.hashes.insert("md5".into(), "aa".into());
        assert_eq!(entry.fragment_hash(), Some(("md5", "aa")));
        entry.hashes.insert("sha256".into(), "bb".into());
        assert_eq!(entry.fragment_hash(), Some(("sha256", "bb")));
    }

    #[test]
    fn root_pages_are_sorted_and_deduplicated() {
        let root = RootPage::from_names(vec!["b".into(), "a".into(), "b".into()]);
        assert_eq!(root.projects, ["a", "b"]);
    }

    #[test]
    fn hex_check() {
        assert!(is_hex("abcDEF0123"));
        assert!(!is_hex(""));
        assert!(!is_hex("xyz"));
    }
}
