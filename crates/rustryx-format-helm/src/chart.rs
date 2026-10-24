//! Reading the metadata out of an uploaded chart archive (`<name>-<version>.tgz`).
//!
//! The archive is hostile input. Nothing from it ever becomes a path on disk or a key in a blob
//! store (the archive was already stored under a name Rustryx chose), so tar slip cannot write
//! anything; it is still refused, because an archive that names `../x` is not a chart and a
//! chart that Rustryx accepts must be one every client can unpack safely. The checks, all run
//! while streaming through the archive once, never holding more than `Chart.yaml`:
//!
//! * the **decompressed size** is capped, and so is the **ratio** of decompressed to compressed
//!   bytes once the output is large enough for the ratio to mean something: a gzip bomb stops
//!   at the cap or at the ratio, whichever comes first, and costs a bounded amount of CPU;
//! * the **entry count** and the **length of each path** are capped;
//! * every path must stay inside the archive: no absolute path, no `..`, no backslash, no NUL,
//!   no control character; links (symbolic and hard) are refused outright;
//! * there is **exactly one top level directory** and `<dir>/Chart.yaml` exists once; the
//!   metadata is read from it with the bounded YAML parser;
//! * the metadata is validated: DNS label name, canonical `SemVer` version, `apiVersion` `v1` or
//!   `v2`, `type` `application` or `library`, bounded text fields.
//!
//! CPU bound: callers run it on the blocking pool behind a permit.

use std::io::Read;
use std::path::Component;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use flate2::read::GzDecoder;

use crate::model::ChartVersion;
use crate::names::{NameError, validate};
use crate::yaml::{YamlError, YamlLimits, parse};

/// Limits of one archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChartLimits {
    /// Largest archive that is read at all, in bytes.
    pub max_archive_bytes: u64,
    /// Most decompressed bytes of the whole archive.
    pub max_decompressed_bytes: u64,
    /// Highest ratio of decompressed to compressed bytes.
    pub max_ratio: u64,
    /// The ratio is only checked once this many bytes were produced: a small archive has a
    /// meaningless ratio (headers and padding dominate).
    pub ratio_floor_bytes: u64,
    /// Most entries.
    pub max_entries: usize,
    /// Longest path of an entry, in bytes.
    pub max_path_len: usize,
    /// Largest `Chart.yaml`, in bytes.
    pub max_chart_yaml_bytes: usize,
}

impl Default for ChartLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 64 * 1024 * 1024,
            max_decompressed_bytes: 128 * 1024 * 1024,
            max_ratio: 100,
            ratio_floor_bytes: 4 * 1024 * 1024,
            max_entries: 10_000,
            max_path_len: 1024,
            max_chart_yaml_bytes: 1024 * 1024,
        }
    }
}

/// Why an archive was refused. The `Display` text is client-safe and names no path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChartError {
    /// The archive is larger than the limit.
    #[error("the chart archive is too large")]
    ArchiveTooLarge,
    /// Not a gzip compressed tar archive.
    #[error("the file is not a gzip compressed tar archive")]
    NotAnArchive,
    /// More decompressed bytes than the limit.
    #[error("the chart archive expands to more than the allowed size")]
    TooLarge,
    /// A compression ratio no honest chart reaches.
    #[error("the chart archive has a suspicious compression ratio")]
    Bomb,
    /// More entries than the limit.
    #[error("the chart archive has too many files")]
    TooManyEntries,
    /// A path that leaves the archive, or is malformed.
    #[error("the chart archive contains an unsafe path")]
    UnsafePath,
    /// A symbolic or hard link.
    #[error("the chart archive contains a link")]
    Link,
    /// Not exactly one top level directory.
    #[error("the chart archive must contain exactly one top level directory")]
    Layout,
    /// No `Chart.yaml` in the chart directory.
    #[error("the chart archive has no Chart.yaml")]
    MissingChartYaml,
    /// `Chart.yaml` twice.
    #[error("the chart archive has more than one Chart.yaml")]
    DuplicateChartYaml,
    /// `Chart.yaml` over its size limit.
    #[error("Chart.yaml is too large")]
    ChartYamlTooLarge,
    /// `Chart.yaml` is not valid YAML or breaks a YAML limit.
    #[error("Chart.yaml is not valid")]
    InvalidYaml,
    /// `Chart.yaml` is valid YAML with an unacceptable chart in it. The text is client-safe.
    #[error("{0}")]
    InvalidMetadata(String),
}

/// What was read from an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartInfo {
    /// The validated metadata of the chart.
    pub metadata: ChartVersion,
    /// Number of entries in the archive.
    pub entries: usize,
    /// Decompressed size of the whole archive.
    pub decompressed_bytes: u64,
}

/// Which limit a counting reader tripped.
const TRIP_NONE: u8 = 0;
const TRIP_SIZE: u8 = 1;
const TRIP_RATIO: u8 = 2;

/// Counts what comes out of the decompressor and stops it at a limit. The reason is stored in
/// a shared cell because the tar reader wraps the I/O error and loses its type.
struct Bounded<R> {
    inner: R,
    produced: u64,
    compressed: u64,
    limits: ChartLimits,
    trip: Arc<AtomicU8>,
}

impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.produced += read as u64;
        let tripped = if self.produced > self.limits.max_decompressed_bytes {
            TRIP_SIZE
        } else if self.produced > self.limits.ratio_floor_bytes
            && self.produced / self.compressed.max(1) > self.limits.max_ratio
        {
            TRIP_RATIO
        } else {
            TRIP_NONE
        };
        if tripped != TRIP_NONE {
            self.trip.store(tripped, Ordering::SeqCst);
            return Err(std::io::Error::other("archive limit exceeded"));
        }
        Ok(read)
    }
}

/// Reads and validates the chart metadata of `archive`.
pub fn read_chart(archive: &[u8], limits: &ChartLimits) -> Result<ChartInfo, ChartError> {
    if archive.len() as u64 > limits.max_archive_bytes {
        return Err(ChartError::ArchiveTooLarge);
    }
    let trip = Arc::new(AtomicU8::new(TRIP_NONE));
    let bounded = Bounded {
        inner: GzDecoder::new(archive),
        produced: 0,
        compressed: archive.len() as u64,
        limits: *limits,
        trip: Arc::clone(&trip),
    };
    let mut tar = tar::Archive::new(bounded);
    let outcome = scan(&mut tar, limits);
    // A tripped limit explains any error the reader produced, whatever the tar layer made of it.
    match trip.load(Ordering::SeqCst) {
        TRIP_SIZE => return Err(ChartError::TooLarge),
        TRIP_RATIO => return Err(ChartError::Bomb),
        _ => {}
    }
    let scanned = outcome?;
    let produced = tar.into_inner().produced;
    let chart_yaml = scanned.chart_yaml.ok_or(ChartError::MissingChartYaml)?;
    let mut metadata: ChartVersion = parse(&chart_yaml, YamlLimits::CHART).map_err(|error| {
        tracing::info!(
            detail = error.detail(),
            "Chart.yaml of an upload was refused"
        );
        match error {
            YamlError::TooLarge { .. } => ChartError::ChartYamlTooLarge,
            _ => ChartError::InvalidYaml,
        }
    })?;
    validate_metadata(&mut metadata)?;
    Ok(ChartInfo {
        metadata,
        entries: scanned.entries,
        decompressed_bytes: produced,
    })
}

struct Scanned {
    entries: usize,
    chart_yaml: Option<Vec<u8>>,
}

fn scan<R: Read>(tar: &mut tar::Archive<R>, limits: &ChartLimits) -> Result<Scanned, ChartError> {
    let entries = tar.entries().map_err(|_| ChartError::NotAnArchive)?;
    let mut root: Option<String> = None;
    let mut count = 0usize;
    let mut chart_yaml: Option<Vec<u8>> = None;
    for entry in entries {
        let mut entry = entry.map_err(|_| ChartError::NotAnArchive)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        count += 1;
        if count > limits.max_entries {
            return Err(ChartError::TooManyEntries);
        }
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(ChartError::Link);
        }
        let path = entry_path(&entry, limits)?;
        let mut parts = path.split('/');
        let top = parts.next().unwrap_or_default();
        let rest: Vec<&str> = parts.collect();
        match &root {
            Some(known) if known != top => return Err(ChartError::Layout),
            Some(_) => {}
            None => root = Some(top.to_owned()),
        }
        if rest.is_empty() && !kind.is_dir() {
            // A file at the root of the archive is not inside a chart directory.
            return Err(ChartError::Layout);
        }
        if kind.is_file() && rest == ["Chart.yaml"] {
            if chart_yaml.is_some() {
                return Err(ChartError::DuplicateChartYaml);
            }
            chart_yaml = Some(read_chart_yaml(&mut entry, limits)?);
        }
    }
    Ok(Scanned {
        entries: count,
        chart_yaml,
    })
}

fn read_chart_yaml<R: Read>(entry: &mut R, limits: &ChartLimits) -> Result<Vec<u8>, ChartError> {
    let cap = limits.max_chart_yaml_bytes as u64;
    let mut bytes = Vec::new();
    entry
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ChartError::NotAnArchive)?;
    if bytes.len() as u64 > cap {
        return Err(ChartError::ChartYamlTooLarge);
    }
    Ok(bytes)
}

/// The path of an entry as a `/` separated string with no `.` parts, after the safety checks.
fn entry_path<R: Read>(
    entry: &tar::Entry<'_, R>,
    limits: &ChartLimits,
) -> Result<String, ChartError> {
    let path = entry.path().map_err(|_| ChartError::UnsafePath)?;
    let text = path.to_str().ok_or(ChartError::UnsafePath)?;
    if text.len() > limits.max_path_len
        || text.contains(['\\', '\0'])
        || text.chars().any(char::is_control)
    {
        return Err(ChartError::UnsafePath);
    }
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => match part.to_str() {
                Some(part) => parts.push(part.to_owned()),
                None => return Err(ChartError::UnsafePath),
            },
            // `./chart/...` is how `tar czf x.tgz ./chart` writes its entries.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ChartError::UnsafePath);
            }
        }
    }
    if parts.is_empty() {
        return Err(ChartError::UnsafePath);
    }
    Ok(parts.join("/"))
}

const MAX_TEXT: usize = 4096;
const MAX_LIST: usize = 64;
const MAX_DEPENDENCIES: usize = 256;
const MAX_ANNOTATIONS: usize = 64;
/// The metadata of one version ends up in every `index.yaml` of the repository: an uploader
/// must not be able to make everybody's index large.
const MAX_METADATA_BYTES: usize = 64 * 1024;

fn invalid(text: &str) -> ChartError {
    ChartError::InvalidMetadata(text.to_owned())
}

fn validate_metadata(metadata: &mut ChartVersion) -> Result<(), ChartError> {
    validate(&metadata.name, &metadata.version).map_err(|error| match error {
        NameError::BadName => ChartError::InvalidMetadata(NameError::BadName.to_string()),
        NameError::BadVersion => ChartError::InvalidMetadata(NameError::BadVersion.to_string()),
    })?;
    match metadata.api_version.as_deref() {
        Some("v1" | "v2") => {}
        _ => return Err(invalid("Chart.yaml apiVersion must be v1 or v2")),
    }
    match metadata.kind.as_deref() {
        None | Some("") => metadata.kind = Some("application".to_owned()),
        Some("application" | "library") => {}
        Some(_) => return Err(invalid("Chart.yaml type must be application or library")),
    }
    // Fields that belong to the index entry are the repository's to write, not the uploader's.
    metadata.urls.clear();
    metadata.created = None;
    metadata.digest = None;
    metadata.removed = false;
    check_text_fields(metadata)?;
    let size = serde_json::to_vec(metadata).map_or(usize::MAX, |bytes| bytes.len());
    if size > MAX_METADATA_BYTES {
        return Err(invalid("Chart.yaml metadata is too large"));
    }
    Ok(())
}

fn check_text_fields(metadata: &ChartVersion) -> Result<(), ChartError> {
    let long = |text: &Option<String>| text.as_deref().is_some_and(|t| t.len() > MAX_TEXT);
    if long(&metadata.description)
        || long(&metadata.app_version)
        || long(&metadata.home)
        || long(&metadata.icon)
        || long(&metadata.kube_version)
    {
        return Err(invalid("a Chart.yaml text field is too long"));
    }
    if metadata.keywords.len() > MAX_LIST
        || metadata.sources.len() > MAX_LIST
        || metadata.maintainers.len() > MAX_LIST
        || metadata.dependencies.len() > MAX_DEPENDENCIES
        || metadata.annotations.len() > MAX_ANNOTATIONS
    {
        return Err(invalid("Chart.yaml has too many list entries"));
    }
    let too_long = metadata
        .keywords
        .iter()
        .chain(&metadata.sources)
        .chain(metadata.annotations.keys())
        .chain(metadata.annotations.values())
        .any(|text| text.len() > MAX_TEXT);
    if too_long {
        return Err(invalid("a Chart.yaml text field is too long"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{archive, chart, chart_yaml, gzip, tar_entry};
    use proptest::prelude::*;
    use std::fmt::Write as _;

    fn read(bytes: &[u8]) -> Result<ChartInfo, ChartError> {
        read_chart(bytes, &ChartLimits::default())
    }

    #[test]
    fn default_limits_are_the_documented_caps() {
        let limits = ChartLimits::default();
        assert_eq!(limits.max_archive_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.max_decompressed_bytes, 128 * 1024 * 1024);
        assert_eq!(limits.max_ratio, 100);
        assert_eq!(limits.max_entries, 10_000);
        assert_eq!(limits.max_chart_yaml_bytes, 1024 * 1024);
    }

    #[test]
    fn reads_the_metadata_of_a_well_formed_chart() {
        let info = read(&chart("web", "1.2.3")).unwrap();
        assert_eq!(info.metadata.name, "web");
        assert_eq!(info.metadata.version, "1.2.3");
        assert_eq!(info.metadata.api_version.as_deref(), Some("v2"));
        assert_eq!(info.metadata.app_version.as_deref(), Some("1.0"));
        assert_eq!(info.metadata.kind.as_deref(), Some("application"));
        assert_eq!(info.entries, 3);
        assert!(info.decompressed_bytes > 0);
    }

    #[test]
    fn index_only_fields_in_chart_yaml_are_dropped() {
        let yaml = format!(
            "{}urls: [https://evil.example/x.tgz]\ncreated: \"2000-01-01T00:00:00Z\"\ndigest: {}\nremoved: true\n",
            chart_yaml("web", "1.0.0"),
            "0".repeat(64)
        );
        let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes())]);
        let metadata = read(&bytes).unwrap().metadata;
        assert!(metadata.urls.is_empty() && metadata.created.is_none());
        assert!(metadata.digest.is_none() && !metadata.removed);
    }

    #[test]
    fn a_missing_type_defaults_to_application_and_library_is_accepted() {
        let yaml = "apiVersion: v2\nname: lib\nversion: 0.1.0\n";
        let bytes = archive(&[("lib/Chart.yaml", yaml.as_bytes())]);
        assert_eq!(
            read(&bytes).unwrap().metadata.kind.as_deref(),
            Some("application")
        );
        let yaml = "apiVersion: v2\nname: lib\nversion: 0.1.0\ntype: library\n";
        let bytes = archive(&[("lib/Chart.yaml", yaml.as_bytes())]);
        assert_eq!(
            read(&bytes).unwrap().metadata.kind.as_deref(),
            Some("library")
        );
    }

    #[test]
    fn a_leading_dot_slash_is_tolerated() {
        let yaml = chart_yaml("web", "1.0.0");
        let bytes = archive(&[
            ("./web/Chart.yaml", yaml.as_bytes()),
            ("./web/values.yaml", b"a: 1\n"),
        ]);
        assert_eq!(read(&bytes).unwrap().metadata.name, "web");
    }

    #[test]
    fn must_not_accept_a_tar_slip_path() {
        let yaml = chart_yaml("web", "1.0.0");
        for evil in [
            "web/../../etc/passwd",
            "../web/values.yaml",
            "/etc/passwd",
            "web/templates/../../../x",
            "web\\..\\x",
        ] {
            let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes()), (evil, b"x")]);
            assert_eq!(read(&bytes), Err(ChartError::UnsafePath), "{evil}");
        }
        // Control characters in a name.
        let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes()), ("web/a\nb", b"x")]);
        assert_eq!(read(&bytes), Err(ChartError::UnsafePath));
    }

    #[test]
    fn must_not_accept_links() {
        let yaml = chart_yaml("web", "1.0.0");
        for kind in *b"21" {
            let mut raw = tar_entry("web/Chart.yaml", yaml.as_bytes(), b'0', "");
            raw.extend_from_slice(&tar_entry("web/link", b"", kind, "/etc/passwd"));
            raw.extend_from_slice(&[0u8; 1024]);
            assert_eq!(read(&gzip(&raw)), Err(ChartError::Link), "kind {kind}");
        }
    }

    #[test]
    fn needs_exactly_one_top_level_directory_with_a_chart_yaml() {
        let yaml = chart_yaml("web", "1.0.0");
        let two = archive(&[
            ("web/Chart.yaml", yaml.as_bytes()),
            ("other/values.yaml", b"a: 1\n"),
        ]);
        assert_eq!(read(&two), Err(ChartError::Layout));
        let at_root = archive(&[("Chart.yaml", yaml.as_bytes())]);
        assert_eq!(read(&at_root), Err(ChartError::Layout));
        let nested = archive(&[("web/sub/Chart.yaml", yaml.as_bytes())]);
        assert_eq!(read(&nested), Err(ChartError::MissingChartYaml));
        let none = archive(&[("web/values.yaml", b"a: 1\n")]);
        assert_eq!(read(&none), Err(ChartError::MissingChartYaml));
        let twice = archive(&[
            ("web/Chart.yaml", yaml.as_bytes()),
            ("web/Chart.yaml", yaml.as_bytes()),
        ]);
        assert_eq!(read(&twice), Err(ChartError::DuplicateChartYaml));
        assert_eq!(read(&archive(&[])), Err(ChartError::MissingChartYaml));
    }

    #[test]
    fn must_not_unpack_a_gzip_bomb() {
        // 200 MiB of zeros compresses to about 200 KiB: far over the ratio, and over the cap.
        let yaml = chart_yaml("web", "1.0.0");
        let zeros = vec![0u8; 200 * 1024 * 1024];
        let mut raw = tar_entry("web/Chart.yaml", yaml.as_bytes(), b'0', "");
        raw.extend_from_slice(&tar_entry("web/big.bin", &zeros, b'0', ""));
        raw.extend_from_slice(&[0u8; 1024]);
        let bomb = gzip(&raw);
        assert!(
            bomb.len() < 1024 * 1024,
            "the bomb is small: {}",
            bomb.len()
        );
        let error = read(&bomb).unwrap_err();
        assert!(
            matches!(error, ChartError::Bomb | ChartError::TooLarge),
            "{error:?}"
        );

        // Under the ratio but over the absolute cap.
        let limits = ChartLimits {
            max_decompressed_bytes: 1024 * 1024,
            ratio_floor_bytes: u64::MAX,
            ..ChartLimits::default()
        };
        let medium = vec![7u8; 2 * 1024 * 1024];
        let mut raw = tar_entry("web/Chart.yaml", yaml.as_bytes(), b'0', "");
        raw.extend_from_slice(&tar_entry("web/blob", &medium, b'0', ""));
        raw.extend_from_slice(&[0u8; 1024]);
        assert_eq!(read_chart(&gzip(&raw), &limits), Err(ChartError::TooLarge));
    }

    #[test]
    fn enforces_the_entry_count_the_path_length_and_the_archive_size() {
        let yaml = chart_yaml("web", "1.0.0");
        let limits = ChartLimits {
            max_entries: 3,
            ..ChartLimits::default()
        };
        let many = archive(&[
            ("web/Chart.yaml", yaml.as_bytes()),
            ("web/a", b"1"),
            ("web/b", b"2"),
            ("web/c", b"3"),
        ]);
        assert_eq!(read_chart(&many, &limits), Err(ChartError::TooManyEntries));
        let long = format!("web/{}", "a".repeat(90));
        let tight = ChartLimits {
            max_path_len: 50,
            ..ChartLimits::default()
        };
        let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes()), (&long, b"x")]);
        assert_eq!(read_chart(&bytes, &tight), Err(ChartError::UnsafePath));
        let small = ChartLimits {
            max_archive_bytes: 10,
            ..ChartLimits::default()
        };
        assert_eq!(
            read_chart(&chart("web", "1.0.0"), &small),
            Err(ChartError::ArchiveTooLarge)
        );
    }

    #[test]
    fn rejects_what_is_not_a_gzip_tar() {
        for bytes in [&b""[..], b"not gzip at all", b"\x1f\x8b\x08\x00garbage"] {
            assert!(
                matches!(
                    read(bytes),
                    Err(ChartError::NotAnArchive | ChartError::MissingChartYaml)
                ),
                "{bytes:?}"
            );
        }
        // A valid gzip of text that is not a tar.
        let text = gzip(b"hello, this is not a tar archive at all");
        assert!(read(&text).is_err());
    }

    #[test]
    fn an_oversized_chart_yaml_is_refused_before_it_is_parsed() {
        let limits = ChartLimits {
            max_chart_yaml_bytes: 100,
            ..ChartLimits::default()
        };
        let yaml = format!("{}# {}\n", chart_yaml("web", "1.0.0"), "x".repeat(200));
        let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes())]);
        assert_eq!(
            read_chart(&bytes, &limits),
            Err(ChartError::ChartYamlTooLarge)
        );
    }

    #[test]
    fn invalid_yaml_and_alias_bombs_in_chart_yaml_are_refused() {
        let bad = archive(&[("web/Chart.yaml", b"name: [unterminated")]);
        assert_eq!(read(&bad), Err(ChartError::InvalidYaml));
        let mut bomb = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for level in 1..12 {
            let refs = vec![format!("*a{}", level - 1); 10].join(", ");
            writeln!(bomb, "a{level}: &a{level} [{refs}]").unwrap();
        }
        bomb.push_str("apiVersion: v2\nname: web\nversion: 1.0.0\n");
        let bytes = archive(&[("web/Chart.yaml", bomb.as_bytes())]);
        assert_eq!(read(&bytes), Err(ChartError::InvalidYaml));
    }

    #[test]
    fn invalid_metadata_is_explained_without_echoing_input() {
        let cases: [(&str, &str); 6] = [
            ("apiVersion: v2\nname: Web\nversion: 1.0.0\n", "chart name"),
            (
                "apiVersion: v2\nname: web\nversion: 1.0\n",
                "semantic version",
            ),
            ("name: web\nversion: 1.0.0\n", "apiVersion"),
            ("apiVersion: v3\nname: web\nversion: 1.0.0\n", "apiVersion"),
            (
                "apiVersion: v2\nname: web\nversion: 1.0.0\ntype: weird\n",
                "type",
            ),
            ("apiVersion: v2\nname: ../x\nversion: 1.0.0\n", "chart name"),
        ];
        for (yaml, expected) in cases {
            let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes())]);
            match read(&bytes) {
                Err(ChartError::InvalidMetadata(message)) => {
                    assert!(message.contains(expected), "{message}");
                }
                other => panic!("{yaml}: {other:?}"),
            }
        }
    }

    #[test]
    fn oversized_metadata_cannot_inflate_everyones_index() {
        let long = "x".repeat(MAX_TEXT + 1);
        let yaml = format!("{}home: {long}\n", chart_yaml("web", "1.0.0"));
        let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes())]);
        assert!(matches!(read(&bytes), Err(ChartError::InvalidMetadata(_))));
        let mut keywords = String::new();
        for i in 0..100 {
            writeln!(keywords, "  - k{i}").unwrap();
        }
        let yaml = format!("{}keywords:\n{keywords}", chart_yaml("web", "1.0.0"));
        let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes())]);
        assert!(matches!(read(&bytes), Err(ChartError::InvalidMetadata(_))));
        let mut deps = String::new();
        for i in 0..20 {
            let filler = "x".repeat(MAX_TEXT);
            write!(deps, "  - name: d{i}\n    description: {filler}\n").unwrap();
        }
        let yaml = format!("{}dependencies:\n{deps}", chart_yaml("web", "1.0.0"));
        let bytes = archive(&[("web/Chart.yaml", yaml.as_bytes())]);
        assert!(
            read(&bytes).is_ok(),
            "unknown dependency keys are dropped, not counted"
        );
    }

    #[test]
    fn error_messages_never_contain_paths_from_the_archive() {
        let yaml = chart_yaml("web", "1.0.0");
        let bytes = archive(&[
            ("web/Chart.yaml", yaml.as_bytes()),
            ("web/../../secret-path", b"x"),
        ]);
        let message = read(&bytes).unwrap_err().to_string();
        assert!(!message.contains("secret-path") && !message.contains(".."));
    }

    proptest! {
        #[test]
        fn reading_arbitrary_bytes_never_panics(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = read(&data);
            // The same bytes as the body of a valid gzip stream.
            let _ = read(&gzip(&data));
        }

        #[test]
        fn a_valid_archive_with_arbitrary_entry_names_never_panics(
            names in proptest::collection::vec("[a-zA-Z0-9./_%-]{1,40}", 0..8)
        ) {
            let yaml = chart_yaml("web", "1.0.0");
            let mut entries: Vec<(&str, &[u8])> = vec![("web/Chart.yaml", yaml.as_bytes())];
            for name in &names {
                entries.push((name.as_str(), b"x"));
            }
            let _ = read(&archive(&entries));
        }
    }
}
