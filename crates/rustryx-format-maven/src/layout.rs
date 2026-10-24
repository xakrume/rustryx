//! The Maven 2 repository layout: `groupId/path/artifactId/version/artifactId-version[-classifier].ext`.
//!
//! Specification: <https://maven.apache.org/repositories/layout.html> (path structure) and
//! <https://maven.apache.org/ref/current/maven-repository-metadata/repository-metadata.html>
//! (`maven-metadata.xml` at group, artifact and version level).
//!
//! [`MavenPath::parse`] is strict and panic free. It accepts exactly the paths that
//! [`MavenPath::to_path_string`] can produce, so `format(parse(p)) == p` for every accepted
//! path. That is a security property, not tidiness: two spellings of one coordinate would be
//! two different assets for one component, and the write-once rule of a release is a rule
//! about a path. The cases that would give a coordinate two spellings are refused:
//!
//! * a dot inside a group directory (`org/acme.lib/...` would be groupId `org.acme.lib`, the
//!   same as `org/acme/lib/...`);
//! * a leading zero in the build number of a timestamped snapshot;
//! * an extension whose first segment is only digits (`a-1.0.1.jar` under version `1.0` is the
//!   file of version `1.0.1`, not extension `1.jar`);
//! * a checksum suffix after a checksum suffix.
//!
//! Known limit: a classifier cannot contain a dot, because the first dot after the classifier
//! starts the extension (`tar.gz` is an extension, `jdk1.5` would be classifier `jdk1`).

use rustryx_core::ports::auth::Coordinate;

use crate::version::is_snapshot;

/// The file name of repository metadata.
pub const METADATA_FILE: &str = "maven-metadata.xml";

/// Longest accepted groupId, artifactId, version, classifier or extension, in bytes.
const MAX_PART_LEN: usize = 128;

/// Why a path is not a Maven 2 layout path. For logs and tests; a client sees one generic message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    /// Too few segments for the layout.
    #[error("path is too short for the maven layout")]
    TooShort,
    /// A group, artifact or version segment has a forbidden character, is empty or too long.
    #[error("invalid coordinate segment")]
    BadSegment,
    /// The file name does not start with `artifactId-version`.
    #[error("file name does not match the artifact and version directories")]
    NameMismatch,
    /// The classifier is empty or has a forbidden character.
    #[error("invalid classifier")]
    BadClassifier,
    /// The extension is missing or not a plausible extension.
    #[error("invalid extension")]
    BadExtension,
    /// The timestamp or build number of a snapshot file is malformed.
    #[error("invalid snapshot timestamp or build number")]
    BadStamp,
    /// A checksum file of a checksum file.
    #[error("a checksum cannot have a checksum")]
    NestedChecksum,
}

/// A digest that a checksum file carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChecksumAlgo {
    /// `.md5`.
    Md5,
    /// `.sha1`.
    Sha1,
    /// `.sha256`.
    Sha256,
    /// `.sha512`.
    Sha512,
}

impl ChecksumAlgo {
    /// Every algorithm, in the order clients commonly ask for them.
    pub const ALL: [Self; 4] = [Self::Sha1, Self::Md5, Self::Sha256, Self::Sha512];

    /// The file suffix without the dot.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    /// Length of the lowercase hex digest.
    #[must_use]
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Md5 => 32,
            Self::Sha1 => 40,
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }

    /// The algorithm of a file name suffix (without the dot).
    #[must_use]
    pub fn from_suffix(suffix: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|algo| algo.suffix() == suffix)
    }

    /// Splits `name.sha1` into `("name", Sha1)`.
    #[must_use]
    pub fn split_file_name(name: &str) -> Option<(&str, Self)> {
        let (stem, suffix) = name.rsplit_once('.')?;
        Self::from_suffix(suffix).map(|algo| (stem, algo))
    }
}

/// The timestamp and build number a unique snapshot deploy puts into the file name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SnapshotStamp {
    /// `yyyyMMdd.HHmmss` (UTC). Text comparison is chronological.
    pub timestamp: String,
    /// Build number, at least 1, no leading zeros in the file name.
    pub build: u32,
}

impl SnapshotStamp {
    /// `timestamp-build`, the text that replaces `SNAPSHOT` in the file name.
    #[must_use]
    pub fn suffix(&self) -> String {
        format!("{}-{}", self.timestamp, self.build)
    }

    /// The `yyyyMMddHHmmss` form used by the `updated` and `lastUpdated` elements.
    #[must_use]
    pub fn compact(&self) -> String {
        self.timestamp.replace('.', "")
    }
}

/// A file inside a version directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPath {
    /// groupId, dotted.
    pub group_id: String,
    /// artifactId.
    pub artifact_id: String,
    /// The version directory (`1.0`, `1.0-SNAPSHOT`), also for a timestamped snapshot file.
    pub version: String,
    /// Classifier, `None` for the main artifact.
    pub classifier: Option<String>,
    /// Extension without the leading dot, possibly with dots (`tar.gz`, `jar.asc`).
    pub extension: String,
    /// Set for a timestamped snapshot file (`a-1.0-20240101.120000-1.jar`).
    pub stamp: Option<SnapshotStamp>,
}

impl ArtifactPath {
    /// The directory of the artifact, `group/path/artifactId`.
    #[must_use]
    pub fn artifact_dir(&self) -> String {
        format!("{}/{}", group_dir(&self.group_id), self.artifact_id)
    }

    /// The directory of the version, `group/path/artifactId/version`.
    #[must_use]
    pub fn version_dir(&self) -> String {
        format!("{}/{}", self.artifact_dir(), self.version)
    }

    /// The file name.
    #[must_use]
    pub fn file_name(&self) -> String {
        let version_part = match (&self.stamp, self.version.strip_suffix("-SNAPSHOT")) {
            (Some(stamp), Some(base)) => format!("{base}-{}", stamp.suffix()),
            _ => self.version.clone(),
        };
        let classifier = self
            .classifier
            .as_deref()
            .map(|c| format!("-{c}"))
            .unwrap_or_default();
        format!(
            "{}-{version_part}{classifier}.{}",
            self.artifact_id, self.extension
        )
    }

    /// The whole path.
    #[must_use]
    pub fn to_path_string(&self) -> String {
        format!("{}/{}", self.version_dir(), self.file_name())
    }

    /// True when the version is a snapshot.
    #[must_use]
    pub fn is_snapshot(&self) -> bool {
        is_snapshot(&self.version)
    }

    /// The same file under another snapshot stamp.
    #[must_use]
    pub fn with_stamp(&self, stamp: SnapshotStamp) -> Self {
        Self {
            stamp: Some(stamp),
            ..self.clone()
        }
    }
}

/// The three levels at which `maven-metadata.xml` exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataLevel {
    /// `group/path/maven-metadata.xml` with one directory: plugin prefixes. With more than one
    /// directory the path is indistinguishable from [`MetadataLevel::Artifact`] and is read as that.
    Group {
        /// groupId.
        group_id: String,
    },
    /// `group/path/artifactId/maven-metadata.xml`: the version list.
    Artifact {
        /// groupId.
        group_id: String,
        /// artifactId.
        artifact_id: String,
    },
    /// `group/path/artifactId/1.0-SNAPSHOT/maven-metadata.xml`: the builds of a snapshot.
    Version {
        /// groupId.
        group_id: String,
        /// artifactId.
        artifact_id: String,
        /// The snapshot version.
        version: String,
    },
}

impl MetadataLevel {
    /// The directory the document lives in.
    #[must_use]
    pub fn dir(&self) -> String {
        match self {
            Self::Group { group_id } => group_dir(group_id),
            Self::Artifact {
                group_id,
                artifact_id,
            } => format!("{}/{artifact_id}", group_dir(group_id)),
            Self::Version {
                group_id,
                artifact_id,
                version,
            } => format!("{}/{artifact_id}/{version}", group_dir(group_id)),
        }
    }

    /// The path of the document.
    #[must_use]
    pub fn document_path(&self) -> String {
        format!("{}/{METADATA_FILE}", self.dir())
    }
}

/// A classified Maven path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MavenPath {
    /// A file of a component, or the checksum file of such a file.
    Artifact {
        /// The file the path names (or the file the checksum describes).
        file: ArtifactPath,
        /// `Some` when the path is `<file>.<algo>`.
        checksum: Option<ChecksumAlgo>,
    },
    /// `maven-metadata.xml`, or its checksum file.
    Metadata {
        /// Which level.
        level: MetadataLevel,
        /// `Some` when the path is `maven-metadata.xml.<algo>`.
        checksum: Option<ChecksumAlgo>,
    },
}

impl MavenPath {
    /// Parses a normalized path (no leading slash, already percent decoded once).
    ///
    /// # Errors
    /// A [`LayoutError`] naming the first rule the path breaks.
    pub fn parse(path: &str) -> Result<Self, LayoutError> {
        let segments: Vec<&str> = path.split('/').collect();
        let (file_name, dirs) = segments.split_last().ok_or(LayoutError::TooShort)?;
        let (name, checksum) = match ChecksumAlgo::split_file_name(file_name) {
            Some((stem, algo)) => {
                if ChecksumAlgo::split_file_name(stem).is_some() {
                    return Err(LayoutError::NestedChecksum);
                }
                (stem, Some(algo))
            }
            None => (*file_name, None),
        };
        if name == METADATA_FILE {
            return parse_metadata(dirs, checksum);
        }
        let [group_dirs @ .., artifact_id, version, _] = segments.as_slice() else {
            return Err(LayoutError::TooShort);
        };
        if group_dirs.is_empty() {
            return Err(LayoutError::TooShort);
        }
        let group_id = join_group(group_dirs)?;
        check_artifact_id(artifact_id)?;
        check_version(version)?;
        let file = parse_file(&group_id, artifact_id, version, name)?;
        Ok(Self::Artifact { file, checksum })
    }

    /// The path text; the inverse of [`MavenPath::parse`].
    #[must_use]
    pub fn to_path_string(&self) -> String {
        match self {
            Self::Artifact { file, checksum } => {
                let base = file.to_path_string();
                match checksum {
                    Some(algo) => format!("{base}.{}", algo.suffix()),
                    None => base,
                }
            }
            Self::Metadata { level, checksum } => {
                let base = level.document_path();
                match checksum {
                    Some(algo) => format!("{base}.{}", algo.suffix()),
                    None => base,
                }
            }
        }
    }

    /// The component coordinate for authorization and routing. `None` is unknown and `Some("")`
    /// is "no classifier" (`docs/rbac.md`): the main artifact passes `Some("")`. A checksum
    /// file carries the coordinate of the file it describes, with the base extension: whoever
    /// may read a file may read its digest. Metadata has no version at artifact level.
    #[must_use]
    pub fn coordinate(&self) -> Coordinate {
        match self {
            Self::Artifact { file, .. } => {
                let mut coordinate = Coordinate::new(
                    Some(file.group_id.clone()),
                    Some(file.artifact_id.clone()),
                    Some(file.version.clone()),
                );
                coordinate.extension = Some(file.extension.clone());
                coordinate.classifier = Some(file.classifier.clone().unwrap_or_default());
                coordinate
            }
            Self::Metadata { level, .. } => {
                let (group, name, version) = match level {
                    MetadataLevel::Group { group_id } => (group_id.clone(), None, None),
                    MetadataLevel::Artifact {
                        group_id,
                        artifact_id,
                    } => (group_id.clone(), Some(artifact_id.clone()), None),
                    MetadataLevel::Version {
                        group_id,
                        artifact_id,
                        version,
                    } => (
                        group_id.clone(),
                        Some(artifact_id.clone()),
                        Some(version.clone()),
                    ),
                };
                let mut coordinate = Coordinate::new(Some(group), name, version);
                coordinate.extension = Some("xml".to_owned());
                coordinate.classifier = Some(String::new());
                coordinate
            }
        }
    }

    /// groupId and artifactId when the path addresses one artifact, for name claims.
    #[must_use]
    pub fn group_and_artifact(&self) -> Option<(&str, &str)> {
        match self {
            Self::Artifact { file, .. } => Some((&file.group_id, &file.artifact_id)),
            Self::Metadata {
                level:
                    MetadataLevel::Artifact {
                        group_id,
                        artifact_id,
                    }
                    | MetadataLevel::Version {
                        group_id,
                        artifact_id,
                        ..
                    },
                ..
            } => Some((group_id, artifact_id)),
            Self::Metadata { .. } => None,
        }
    }

    /// The version directory name, when the path has one.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Artifact { file, .. } => Some(&file.version),
            Self::Metadata {
                level: MetadataLevel::Version { version, .. },
                ..
            } => Some(version),
            Self::Metadata { .. } => None,
        }
    }

    /// True when the path is a checksum file.
    #[must_use]
    pub fn checksum(&self) -> Option<ChecksumAlgo> {
        match self {
            Self::Artifact { checksum, .. } | Self::Metadata { checksum, .. } => *checksum,
        }
    }
}

/// Splits `group/path/artifactId/version` (a version directory, as in a `DELETE` of a whole
/// version) into `(groupId, artifactId, version)`.
///
/// # Errors
/// [`LayoutError::TooShort`] without a group directory, [`LayoutError::BadSegment`] for a
/// segment outside the coordinate alphabet.
pub fn parse_version_dir(path: &str) -> Result<(String, String, String), LayoutError> {
    let segments: Vec<&str> = path.split('/').collect();
    let [group_dirs @ .., artifact_id, version] = segments.as_slice() else {
        return Err(LayoutError::TooShort);
    };
    if group_dirs.is_empty() {
        return Err(LayoutError::TooShort);
    }
    let group_id = join_group(group_dirs)?;
    check_artifact_id(artifact_id)?;
    check_version(version)?;
    Ok((group_id, (*artifact_id).to_owned(), (*version).to_owned()))
}

/// The stamp inside a `snapshotVersion` value (`1.0-20240102.030405-3` for the version
/// `1.0-SNAPSHOT`). `None` when the value is not of that shape (a non unique snapshot lists the
/// version itself).
#[must_use]
pub fn parse_snapshot_value(version: &str, value: &str) -> Option<SnapshotStamp> {
    let base = version
        .strip_suffix("-SNAPSHOT")
        .filter(|b| !b.is_empty())?;
    let rest = value.strip_prefix(base)?.strip_prefix('-')?;
    match split_stamp(rest) {
        Ok((stamp, "")) => Some(stamp),
        _ => None,
    }
}

fn parse_metadata(dirs: &[&str], checksum: Option<ChecksumAlgo>) -> Result<MavenPath, LayoutError> {
    let level = match dirs {
        [] => return Err(LayoutError::TooShort),
        [only] => {
            check_group_dir(only)?;
            MetadataLevel::Group {
                group_id: (*only).to_owned(),
            }
        }
        [group_dirs @ .., artifact_id, version]
            if is_snapshot(version) && !group_dirs.is_empty() =>
        {
            check_artifact_id(artifact_id)?;
            check_version(version)?;
            MetadataLevel::Version {
                group_id: join_group(group_dirs)?,
                artifact_id: (*artifact_id).to_owned(),
                version: (*version).to_owned(),
            }
        }
        [group_dirs @ .., artifact_id] => {
            check_artifact_id(artifact_id)?;
            MetadataLevel::Artifact {
                group_id: join_group(group_dirs)?,
                artifact_id: (*artifact_id).to_owned(),
            }
        }
    };
    Ok(MavenPath::Metadata { level, checksum })
}

fn group_dir(group_id: &str) -> String {
    group_id.replace('.', "/")
}

fn join_group(dirs: &[&str]) -> Result<String, LayoutError> {
    for dir in dirs {
        check_group_dir(dir)?;
    }
    Ok(dirs.join("."))
}

fn within_len(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_PART_LEN
}

fn check_group_dir(dir: &str) -> Result<(), LayoutError> {
    let ok = within_len(dir)
        && dir
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(LayoutError::BadSegment)
    }
}

fn check_artifact_id(id: &str) -> Result<(), LayoutError> {
    let ok = within_len(id)
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'+'));
    if ok {
        Ok(())
    } else {
        Err(LayoutError::BadSegment)
    }
}

fn check_version(version: &str) -> Result<(), LayoutError> {
    // The same alphabet as an artifactId: versions such as `1.0.0+build.5` are legal.
    check_artifact_id(version)
}

fn check_classifier(classifier: &str) -> Result<(), LayoutError> {
    let ok = within_len(classifier)
        && classifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+'));
    if ok {
        Ok(())
    } else {
        Err(LayoutError::BadClassifier)
    }
}

fn check_extension(extension: &str) -> Result<(), LayoutError> {
    if !within_len(extension) {
        return Err(LayoutError::BadExtension);
    }
    let mut segments = extension.split('.');
    let first = segments.next().unwrap_or_default();
    // A numeric first segment means the file name continues the version, not an extension.
    if !first.bytes().any(|b| b.is_ascii_alphabetic()) {
        return Err(LayoutError::BadExtension);
    }
    let all_ok = extension.split('.').all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    });
    if all_ok {
        Ok(())
    } else {
        Err(LayoutError::BadExtension)
    }
}

/// `.ext` or `-classifier.ext`.
fn parse_tail(tail: &str) -> Result<(Option<String>, String), LayoutError> {
    if let Some(extension) = tail.strip_prefix('.') {
        check_extension(extension)?;
        return Ok((None, extension.to_owned()));
    }
    let after = tail.strip_prefix('-').ok_or(LayoutError::NameMismatch)?;
    let (classifier, extension) = after.split_once('.').ok_or(LayoutError::BadExtension)?;
    check_classifier(classifier)?;
    check_extension(extension)?;
    Ok((Some(classifier.to_owned()), extension.to_owned()))
}

fn parse_file(
    group_id: &str,
    artifact_id: &str,
    version: &str,
    name: &str,
) -> Result<ArtifactPath, LayoutError> {
    let rest = name
        .strip_prefix(artifact_id)
        .and_then(|r| r.strip_prefix('-'))
        .ok_or(LayoutError::NameMismatch)?;
    let build = |classifier, extension, stamp| ArtifactPath {
        group_id: group_id.to_owned(),
        artifact_id: artifact_id.to_owned(),
        version: version.to_owned(),
        classifier,
        extension,
        stamp,
    };
    // The plain form first: `a-1.0.jar`, and the non-unique snapshot `a-1.0-SNAPSHOT.jar`.
    let plain_error = match rest.strip_prefix(version).map(parse_tail) {
        Some(Ok((classifier, extension))) => return Ok(build(classifier, extension, None)),
        Some(Err(error)) => error,
        None => LayoutError::NameMismatch,
    };
    let Some(base) = version.strip_suffix("-SNAPSHOT").filter(|b| !b.is_empty()) else {
        return Err(plain_error);
    };
    let stamped = rest
        .strip_prefix(base)
        .and_then(|r| r.strip_prefix('-'))
        .ok_or(plain_error)?;
    let (stamp, tail) = split_stamp(stamped)?;
    let (classifier, extension) = parse_tail(tail)?;
    Ok(build(classifier, extension, Some(stamp)))
}

/// `yyyyMMdd.HHmmss-N` at the start of `text`; returns the stamp and what follows.
fn split_stamp(text: &str) -> Result<(SnapshotStamp, &str), LayoutError> {
    let bytes = text.as_bytes();
    let digits = |from: usize, to: usize| -> Option<u32> {
        let slice = bytes.get(from..to)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    let (year, month, day) = (digits(0, 4), digits(4, 6), digits(6, 8));
    let (hour, minute, second) = (digits(9, 11), digits(11, 13), digits(13, 15));
    let separators_ok = bytes.get(8) == Some(&b'.') && bytes.get(15) == Some(&b'-');
    let in_range =
        |value: Option<u32>, low: u32, high: u32| value.is_some_and(|v| (low..=high).contains(&v));
    if !(separators_ok
        && year.is_some()
        && in_range(month, 1, 12)
        && in_range(day, 1, 31)
        && in_range(hour, 0, 23)
        && in_range(minute, 0, 59)
        && in_range(second, 0, 60))
    {
        return Err(LayoutError::BadStamp);
    }
    let after = text.get(16..).ok_or(LayoutError::BadStamp)?;
    let digit_count = after.bytes().take_while(u8::is_ascii_digit).count();
    let (build_text, tail) = after
        .split_at_checked(digit_count)
        .ok_or(LayoutError::BadStamp)?;
    if build_text.is_empty() || build_text.starts_with('0') {
        return Err(LayoutError::BadStamp);
    }
    let build: u32 = build_text.parse().map_err(|_| LayoutError::BadStamp)?;
    let timestamp = text.get(..15).ok_or(LayoutError::BadStamp)?.to_owned();
    Ok((SnapshotStamp { timestamp, build }, tail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn artifact(path: &str) -> (ArtifactPath, Option<ChecksumAlgo>) {
        match MavenPath::parse(path) {
            Ok(MavenPath::Artifact { file, checksum }) => (file, checksum),
            other => panic!("{path}: {other:?}"),
        }
    }

    #[test]
    fn a_release_artifact_has_group_artifact_version_and_extension() {
        let (file, checksum) = artifact("org/acme/lib/1.0/lib-1.0.jar");
        assert_eq!(file.group_id, "org.acme");
        assert_eq!(file.artifact_id, "lib");
        assert_eq!(file.version, "1.0");
        assert_eq!(file.classifier, None);
        assert_eq!(file.extension, "jar");
        assert_eq!(file.stamp, None);
        assert_eq!(checksum, None);
        assert!(!file.is_snapshot());
    }

    #[test]
    fn classifier_and_multi_dot_extension() {
        let (file, _) = artifact("org/acme/lib/1.0/lib-1.0-sources.jar");
        assert_eq!(file.classifier.as_deref(), Some("sources"));
        assert_eq!(file.extension, "jar");
        let (file, _) = artifact("org/acme/lib/1.0/lib-1.0-bin.tar.gz");
        assert_eq!(file.classifier.as_deref(), Some("bin"));
        assert_eq!(file.extension, "tar.gz");
        let (file, _) = artifact("org/acme/lib/1.0/lib-1.0.tar.gz");
        assert_eq!(file.classifier, None);
        assert_eq!(file.extension, "tar.gz");
        let (file, _) = artifact("org/acme/lib/1.0/lib-1.0.jar.asc");
        assert_eq!(file.extension, "jar.asc");
    }

    #[test]
    fn checksum_files_describe_the_file_before_the_suffix() {
        for algo in ChecksumAlgo::ALL {
            let path = format!("org/acme/lib/1.0/lib-1.0.pom.{}", algo.suffix());
            let (file, checksum) = artifact(&path);
            assert_eq!(file.extension, "pom");
            assert_eq!(checksum, Some(algo));
            assert_eq!(MavenPath::parse(&path).unwrap().to_path_string(), path);
        }
    }

    #[test]
    fn a_timestamped_snapshot_file_keeps_the_base_version_directory() {
        let (file, _) = artifact("org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405-7-sources.jar");
        assert_eq!(file.version, "1.0-SNAPSHOT");
        assert_eq!(file.classifier.as_deref(), Some("sources"));
        let stamp = file.stamp.clone().unwrap();
        assert_eq!(stamp.timestamp, "20240102.030405");
        assert_eq!(stamp.build, 7);
        assert_eq!(stamp.compact(), "20240102030405");
        assert!(file.is_snapshot());
    }

    #[test]
    fn a_non_unique_snapshot_file_has_no_stamp() {
        let (file, _) = artifact("org/acme/lib/1.0-SNAPSHOT/lib-1.0-SNAPSHOT.jar");
        assert_eq!(file.stamp, None);
        assert_eq!(file.version, "1.0-SNAPSHOT");
    }

    #[test]
    fn metadata_levels() {
        assert_eq!(
            MavenPath::parse("org/acme/lib/maven-metadata.xml").unwrap(),
            MavenPath::Metadata {
                level: MetadataLevel::Artifact {
                    group_id: "org.acme".into(),
                    artifact_id: "lib".into()
                },
                checksum: None
            }
        );
        assert_eq!(
            MavenPath::parse("org/acme/lib/1.0-SNAPSHOT/maven-metadata.xml.sha1").unwrap(),
            MavenPath::Metadata {
                level: MetadataLevel::Version {
                    group_id: "org.acme".into(),
                    artifact_id: "lib".into(),
                    version: "1.0-SNAPSHOT".into()
                },
                checksum: Some(ChecksumAlgo::Sha1)
            }
        );
        assert_eq!(
            MavenPath::parse("junit/maven-metadata.xml").unwrap(),
            MavenPath::Metadata {
                level: MetadataLevel::Group {
                    group_id: "junit".into()
                },
                checksum: None
            }
        );
    }

    #[test]
    fn the_coordinate_passes_an_empty_classifier_for_the_main_artifact() {
        let coordinate = MavenPath::parse("org/acme/lib/1.0/lib-1.0.jar")
            .unwrap()
            .coordinate();
        assert_eq!(coordinate.group.as_deref(), Some("org.acme"));
        assert_eq!(coordinate.name.as_deref(), Some("lib"));
        assert_eq!(coordinate.version.as_deref(), Some("1.0"));
        assert_eq!(coordinate.extension.as_deref(), Some("jar"));
        assert_eq!(coordinate.classifier.as_deref(), Some(""));
        let sources = MavenPath::parse("org/acme/lib/1.0/lib-1.0-sources.jar.sha1")
            .unwrap()
            .coordinate();
        assert_eq!(sources.classifier.as_deref(), Some("sources"));
        assert_eq!(sources.extension.as_deref(), Some("jar"));
        let metadata = MavenPath::parse("org/acme/lib/maven-metadata.xml")
            .unwrap()
            .coordinate();
        assert_eq!(metadata.version, None);
        assert_eq!(metadata.extension.as_deref(), Some("xml"));
    }

    #[test]
    fn group_and_artifact_for_claims() {
        for path in [
            "org/acme/lib/1.0/lib-1.0.jar",
            "org/acme/lib/maven-metadata.xml",
            "org/acme/lib/1.0-SNAPSHOT/maven-metadata.xml.md5",
        ] {
            assert_eq!(
                MavenPath::parse(path).unwrap().group_and_artifact(),
                Some(("org.acme", "lib")),
                "{path}"
            );
        }
        assert_eq!(
            MavenPath::parse("junit/maven-metadata.xml")
                .unwrap()
                .group_and_artifact(),
            None
        );
    }

    #[test]
    fn must_not_accept_a_second_spelling_of_a_coordinate() {
        for bad in [
            // a dot inside a group directory: the same groupId as org/acme/lib
            "org/acme.lib/x/1.0/x-1.0.jar",
            // leading zero in the build number
            "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405-07.jar",
            // build number zero
            "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405-0.jar",
            // numeric extension: this is a file of version 1.0.1
            "org/acme/lib/1.0/lib-1.0.1.jar",
            // checksum of a checksum
            "org/acme/lib/1.0/lib-1.0.jar.sha1.sha1",
            "org/acme/lib/maven-metadata.xml.sha1.md5",
        ] {
            assert!(MavenPath::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_malformed_layouts() {
        for bad in [
            "",
            "a",
            "a/b",
            "a/b/c",
            "org/acme/lib/1.0/other-1.0.jar",
            "org/acme/lib/1.0/lib-2.0.jar",
            "org/acme/lib/1.0/lib-1.0",
            "org/acme/lib/1.0/lib-1.0.",
            "org/acme/lib/1.0/lib-1.0-.jar",
            "org/acme/lib/1.0/lib-1.0-src",
            "org/acme/lib/1.0/lib-1.0.j ar",
            "org/ac me/lib/1.0/lib-1.0.jar",
            "org/acme/lib/1.0/lib-1.0.jar/",
            "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20241302.030405-1.jar",
            "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405.jar",
            "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102x030405-1.jar",
            "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405-99999999999.jar",
            "maven-metadata.xml",
            "org/acme/l\u{e9}/1.0/l\u{e9}-1.0.jar",
        ] {
            assert!(MavenPath::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parts_are_length_limited() {
        let long = "a".repeat(MAX_PART_LEN + 1);
        let path = format!("org/{long}/1.0/{long}-1.0.jar");
        assert!(MavenPath::parse(&path).is_err());
    }

    #[test]
    fn with_stamp_builds_the_timestamped_file() {
        let (file, _) = artifact("org/acme/lib/1.0-SNAPSHOT/lib-1.0-SNAPSHOT.jar");
        let stamped = file.with_stamp(SnapshotStamp {
            timestamp: "20240102.030405".into(),
            build: 3,
        });
        assert_eq!(
            stamped.to_path_string(),
            "org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405-3.jar"
        );
    }

    #[test]
    fn version_directories_and_snapshot_values() {
        assert_eq!(
            parse_version_dir("org/acme/lib/1.0").unwrap(),
            ("org.acme".to_owned(), "lib".to_owned(), "1.0".to_owned())
        );
        assert!(parse_version_dir("lib/1.0").is_err());
        assert!(parse_version_dir("org/ac me/lib/1.0").is_err());
        assert!(parse_version_dir("").is_err());
        let stamp = parse_snapshot_value("1.0-SNAPSHOT", "1.0-20240102.030405-3").unwrap();
        assert_eq!(
            (stamp.timestamp.as_str(), stamp.build),
            ("20240102.030405", 3)
        );
        assert_eq!(parse_snapshot_value("1.0-SNAPSHOT", "1.0-SNAPSHOT"), None);
        assert_eq!(
            parse_snapshot_value("1.0-SNAPSHOT", "2.0-20240102.030405-3"),
            None
        );
        assert_eq!(
            parse_snapshot_value("1.0-SNAPSHOT", "1.0-20240102.030405-3x"),
            None
        );
        assert_eq!(parse_snapshot_value("1.0", "1.0-20240102.030405-3"), None);
    }

    #[test]
    fn version_accessor_and_checksum_accessor() {
        let path = MavenPath::parse("org/acme/lib/1.0-SNAPSHOT/maven-metadata.xml").unwrap();
        assert_eq!(path.version(), Some("1.0-SNAPSHOT"));
        assert_eq!(path.checksum(), None);
        let path = MavenPath::parse("org/acme/lib/maven-metadata.xml.sha512").unwrap();
        assert_eq!(path.version(), None);
        assert_eq!(path.checksum(), Some(ChecksumAlgo::Sha512));
        assert_eq!(ChecksumAlgo::Sha512.hex_len(), 128);
        assert_eq!(ChecksumAlgo::from_suffix("sha1"), Some(ChecksumAlgo::Sha1));
        assert_eq!(ChecksumAlgo::from_suffix("asc"), None);
    }

    fn segment() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_-]{0,8}"
    }

    fn artifact_id() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_.+-]{0,10}"
    }

    fn version() -> impl Strategy<Value = String> {
        prop_oneof![
            "[0-9][0-9a-z.+-]{0,8}",
            "[0-9][0-9a-z.]{0,5}".prop_map(|base| format!("{base}-SNAPSHOT")),
        ]
    }

    fn extension() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("jar".to_owned()),
            Just("pom".to_owned()),
            Just("tar.gz".to_owned()),
            "[a-z][a-z0-9]{0,5}",
        ]
    }

    prop_compose! {
        fn artifact_path()(
            groups in proptest::collection::vec(segment(), 1..4),
            artifact in artifact_id(),
            version in version(),
            classifier in proptest::option::of("[a-z][a-z0-9_+-]{0,8}"),
            extension in extension(),
            stamped in any::<bool>(),
            build in 1u32..100_000,
        ) -> ArtifactPath {
            let stamp = (stamped && is_snapshot(&version)).then(|| SnapshotStamp {
                timestamp: "20240102.030405".to_owned(),
                build,
            });
            ArtifactPath {
                group_id: groups.join("."),
                artifact_id: artifact,
                version,
                classifier,
                extension,
                stamp,
            }
        }
    }

    proptest! {
        #[test]
        fn format_then_parse_round_trips(file in artifact_path()) {
            let text = file.to_path_string();
            match MavenPath::parse(&text) {
                Ok(MavenPath::Artifact { file: parsed, checksum: None }) => prop_assert_eq!(parsed, file),
                other => prop_assert!(false, "{text}: {other:?}"),
            }
        }

        #[test]
        fn parse_then_format_is_the_identity_on_accepted_paths(text in "[a-z0-9./_+-]{0,60}") {
            if let Ok(parsed) = MavenPath::parse(&text) {
                prop_assert_eq!(parsed.to_path_string(), text);
            }
        }

        #[test]
        fn must_not_panic_on_arbitrary_text(text in ".{0,200}") {
            let _ = MavenPath::parse(&text);
        }

        #[test]
        fn checksum_suffix_round_trips(file in artifact_path(), index in 0usize..4) {
            let algo = ChecksumAlgo::ALL[index];
            let text = format!("{}.{}", file.to_path_string(), algo.suffix());
            match MavenPath::parse(&text) {
                Ok(MavenPath::Artifact { file: parsed, checksum }) => {
                    prop_assert_eq!(parsed, file);
                    prop_assert_eq!(checksum, Some(algo));
                }
                other => prop_assert!(false, "{text}: {other:?}"),
            }
        }
    }
}
