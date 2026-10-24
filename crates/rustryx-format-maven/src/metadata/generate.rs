//! Generation of `maven-metadata.xml` from what a hosted repository stores.
//!
//! The metadata of a hosted repository is derived state: the version list is the list of
//! components, the build list of a snapshot is the set of timestamped files in its directory.
//! Deriving it on every read (instead of storing a document that every deploy rewrites) is
//! what makes it work under `allow_once`, where the metadata port refuses a second write of
//! any path, and what keeps it identical on every node. The functions here are pure; the
//! handler feeds them from the ports.

use time::OffsetDateTime;
use time::macros::format_description;

use super::{Metadata, Snapshot, SnapshotVersion, Versioning};
use crate::layout::SnapshotStamp;
use crate::version::{is_snapshot, max_version, sort_versions};

/// One version of an artifact and when it last changed.
#[derive(Debug, Clone)]
pub struct VersionRecord {
    /// The version directory name.
    pub version: String,
    /// Last change of the component (creation or update, whichever is later).
    pub changed: OffsetDateTime,
}

/// One timestamped file of a snapshot version.
#[derive(Debug, Clone)]
pub struct BuildAsset {
    /// Classifier, `None` for the main artifact.
    pub classifier: Option<String>,
    /// Extension.
    pub extension: String,
    /// The stamp in the file name.
    pub stamp: SnapshotStamp,
}

/// `yyyyMMddHHmmss` in UTC.
fn compact(at: OffsetDateTime) -> String {
    let format = format_description!("[year][month][day][hour][minute][second]");
    at.to_offset(time::UtcOffset::UTC)
        .format(&format)
        .unwrap_or_default()
}

/// Artifact level document: all versions in Maven order, `latest` (highest, snapshots
/// included), `release` (highest non snapshot) and `lastUpdated` (newest change).
///
/// `latest` is the highest version in Maven order, not the most recently deployed one, so
/// that it is the same on every node and can be recomputed by a group merge from the version
/// lists alone.
///
/// `None` for an artifact with no version: there is nothing to describe, the document is 404.
#[must_use]
pub fn generate_artifact_metadata(
    group_id: &str,
    artifact_id: &str,
    records: &[VersionRecord],
) -> Option<Metadata> {
    let newest = records.iter().map(|r| r.changed).max()?;
    let mut versions: Vec<String> = records.iter().map(|r| r.version.clone()).collect();
    versions.sort();
    versions.dedup();
    sort_versions(&mut versions);
    let latest = max_version(versions.iter().map(String::as_str)).map(str::to_owned);
    let release = max_version(
        versions
            .iter()
            .map(String::as_str)
            .filter(|v| !is_snapshot(v)),
    )
    .map(str::to_owned);
    Some(Metadata {
        group_id: Some(group_id.to_owned()),
        artifact_id: Some(artifact_id.to_owned()),
        version: None,
        versioning: Some(Versioning {
            latest,
            release,
            versions,
            last_updated: Some(compact(newest)),
            snapshot: None,
            snapshot_versions: Vec::new(),
        }),
        plugins: Vec::new(),
    })
}

/// Version level document of a snapshot: the newest build and, per classifier and extension,
/// the newest file. `None` when no timestamped file exists (a non unique snapshot has nothing
/// to resolve, a client asks for `artifact-1.0-SNAPSHOT.jar` directly).
#[must_use]
pub fn generate_version_metadata(
    group_id: &str,
    artifact_id: &str,
    version: &str,
    builds: &[BuildAsset],
) -> Option<Metadata> {
    let base = version
        .strip_suffix("-SNAPSHOT")
        .filter(|b| !b.is_empty())?;
    let newest = builds.iter().map(|b| &b.stamp).max()?;
    let mut best: Vec<&BuildAsset> = Vec::new();
    for build in builds {
        match best
            .iter_mut()
            .find(|b| b.classifier == build.classifier && b.extension == build.extension)
        {
            Some(slot) => {
                if build.stamp > slot.stamp {
                    *slot = build;
                }
            }
            None => best.push(build),
        }
    }
    best.sort_by(|a, b| {
        a.classifier
            .cmp(&b.classifier)
            .then_with(|| a.extension.cmp(&b.extension))
    });
    let snapshot_versions = best
        .into_iter()
        .map(|b| SnapshotVersion {
            classifier: b.classifier.clone(),
            extension: b.extension.clone(),
            value: format!("{base}-{}", b.stamp.suffix()),
            updated: Some(b.stamp.compact()),
        })
        .collect();
    Some(Metadata {
        group_id: Some(group_id.to_owned()),
        artifact_id: Some(artifact_id.to_owned()),
        version: Some(version.to_owned()),
        versioning: Some(Versioning {
            snapshot: Some(Snapshot {
                timestamp: Some(newest.timestamp.clone()),
                build_number: Some(newest.build),
            }),
            last_updated: Some(newest.compact()),
            snapshot_versions,
            ..Versioning::default()
        }),
        plugins: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(unix: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(unix).unwrap()
    }

    fn stamp(timestamp: &str, build: u32) -> SnapshotStamp {
        SnapshotStamp {
            timestamp: timestamp.to_owned(),
            build,
        }
    }

    #[test]
    fn artifact_metadata_sorts_versions_and_computes_latest_and_release() {
        let records = [
            VersionRecord {
                version: "1.10".into(),
                changed: at(1_700_000_100),
            },
            VersionRecord {
                version: "1.9".into(),
                changed: at(1_700_000_000),
            },
            VersionRecord {
                version: "2.0-SNAPSHOT".into(),
                changed: at(1_700_000_200),
            },
            VersionRecord {
                version: "1.9".into(),
                changed: at(1_700_000_050),
            },
        ];
        let metadata = generate_artifact_metadata("org.acme", "lib", &records).unwrap();
        let v = metadata.versioning.unwrap();
        assert_eq!(v.versions, ["1.9", "1.10", "2.0-SNAPSHOT"]);
        assert_eq!(v.latest.as_deref(), Some("2.0-SNAPSHOT"));
        assert_eq!(v.release.as_deref(), Some("1.10"));
        assert_eq!(v.last_updated.as_deref(), Some("20231114221640"));
    }

    #[test]
    fn a_snapshot_only_artifact_has_no_release() {
        let records = [VersionRecord {
            version: "1.0-SNAPSHOT".into(),
            changed: at(1_700_000_000),
        }];
        let v = generate_artifact_metadata("g", "a", &records)
            .unwrap()
            .versioning
            .unwrap();
        assert_eq!(v.release, None);
        assert_eq!(v.latest.as_deref(), Some("1.0-SNAPSHOT"));
    }

    #[test]
    fn no_versions_means_no_document() {
        assert!(generate_artifact_metadata("g", "a", &[]).is_none());
        assert!(generate_version_metadata("g", "a", "1.0-SNAPSHOT", &[]).is_none());
        assert!(
            generate_version_metadata(
                "g",
                "a",
                "1.0",
                &[BuildAsset {
                    classifier: None,
                    extension: "jar".into(),
                    stamp: stamp("20240102.030405", 1),
                }]
            )
            .is_none()
        );
    }

    #[test]
    fn version_metadata_takes_the_newest_build_per_classifier_and_extension() {
        let builds = [
            BuildAsset {
                classifier: None,
                extension: "jar".into(),
                stamp: stamp("20240102.030405", 1),
            },
            BuildAsset {
                classifier: None,
                extension: "jar".into(),
                stamp: stamp("20240103.030405", 2),
            },
            BuildAsset {
                classifier: None,
                extension: "pom".into(),
                stamp: stamp("20240103.030405", 2),
            },
            BuildAsset {
                classifier: Some("sources".into()),
                extension: "jar".into(),
                stamp: stamp("20240102.030405", 1),
            },
        ];
        let metadata =
            generate_version_metadata("org.acme", "lib", "1.0-SNAPSHOT", &builds).unwrap();
        assert_eq!(metadata.version.as_deref(), Some("1.0-SNAPSHOT"));
        let v = metadata.versioning.unwrap();
        let snapshot = v.snapshot.unwrap();
        assert_eq!(snapshot.timestamp.as_deref(), Some("20240103.030405"));
        assert_eq!(snapshot.build_number, Some(2));
        assert_eq!(v.last_updated.as_deref(), Some("20240103030405"));
        let entries: Vec<(Option<&str>, &str, &str)> = v
            .snapshot_versions
            .iter()
            .map(|e| {
                (
                    e.classifier.as_deref(),
                    e.extension.as_str(),
                    e.value.as_str(),
                )
            })
            .collect();
        assert_eq!(
            entries,
            [
                (None, "jar", "1.0-20240103.030405-2"),
                (None, "pom", "1.0-20240103.030405-2"),
                (Some("sources"), "jar", "1.0-20240102.030405-1"),
            ]
        );
    }

    #[test]
    fn a_higher_build_number_wins_within_one_timestamp() {
        let builds = [
            BuildAsset {
                classifier: None,
                extension: "jar".into(),
                stamp: stamp("20240102.030405", 9),
            },
            BuildAsset {
                classifier: None,
                extension: "jar".into(),
                stamp: stamp("20240102.030405", 10),
            },
        ];
        let v = generate_version_metadata("g", "a", "1.0-SNAPSHOT", &builds)
            .unwrap()
            .versioning
            .unwrap();
        assert_eq!(v.snapshot.unwrap().build_number, Some(10));
    }
}
