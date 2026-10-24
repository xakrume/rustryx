//! Merge of the `maven-metadata.xml` documents of the members of a group.
//!
//! Rules (they are the format's, `docs/format-core.md` section 3 leaves them to it):
//!
//! * **Artifact level**: the union of the version lists, in Maven order; `latest` and
//!   `release` are recomputed from the union (a member's own values are not trusted, a member
//!   that lists no versions contributes nothing); `lastUpdated` is the newest of the members.
//! * **Version level (snapshot)**: the member with the newest build (`timestamp`, then
//!   `buildNumber`) wins as a whole, ties go to the earlier member of the group order. Mixing
//!   entries of two members would describe a build that no single repository has: the `jar`
//!   of build 5 with the `pom` of build 3. The files named by the winner exist in the winner,
//!   and a group serves a file from the first member that has it.
//! * **Group level (plugin prefixes)**: the union of `plugin` entries by prefix and
//!   artifactId, the first member that lists one wins.
//!
//! The group id, artifact id and version of the result come from the request path, never from
//! a member document: a member cannot rename what the group answers.

use super::{Metadata, Plugin, Snapshot, Versioning};
use crate::layout::MetadataLevel;
use crate::version::{is_snapshot, max_version, sort_versions};

/// Merges parsed member documents, given in group order. `None` when no document is usable
/// for the level (for example no member has a snapshot build).
#[must_use]
pub fn merge_documents(level: &MetadataLevel, documents: &[Metadata]) -> Option<Metadata> {
    let plugin_mode = matches!(level, MetadataLevel::Group { .. })
        || (documents.iter().all(|d| d.artifact_id.is_none())
            && documents.iter().any(|d| !d.plugins.is_empty()));
    match level {
        _ if plugin_mode => merge_plugins(level, documents),
        MetadataLevel::Artifact {
            group_id,
            artifact_id,
        } => merge_versions(group_id, artifact_id, documents),
        MetadataLevel::Version {
            group_id,
            artifact_id,
            version,
        } => merge_snapshot(group_id, artifact_id, version, documents),
        MetadataLevel::Group { .. } => None,
    }
}

fn merge_versions(group_id: &str, artifact_id: &str, documents: &[Metadata]) -> Option<Metadata> {
    let mut versions: Vec<String> = documents
        .iter()
        .filter_map(|d| d.versioning.as_ref())
        .flat_map(|v| v.versions.iter().cloned())
        .collect();
    if versions.is_empty() {
        return None;
    }
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
    let last_updated = documents
        .iter()
        .filter_map(|d| d.versioning.as_ref()?.last_updated.clone())
        .max();
    Some(Metadata {
        group_id: Some(group_id.to_owned()),
        artifact_id: Some(artifact_id.to_owned()),
        version: None,
        versioning: Some(Versioning {
            latest,
            release,
            versions,
            last_updated,
            snapshot: None,
            snapshot_versions: Vec::new(),
        }),
        plugins: Vec::new(),
    })
}

fn build_key(snapshot: &Snapshot) -> Option<(&str, u32)> {
    Some((snapshot.timestamp.as_deref()?, snapshot.build_number?))
}

fn merge_snapshot(
    group_id: &str,
    artifact_id: &str,
    version: &str,
    documents: &[Metadata],
) -> Option<Metadata> {
    let mut winner: Option<(&Versioning, (&str, u32))> = None;
    for document in documents {
        // A document for another version must not answer for this one.
        if document.version.as_deref().is_some_and(|v| v != version) {
            continue;
        }
        let Some(versioning) = &document.versioning else {
            continue;
        };
        let Some(key) = versioning.snapshot.as_ref().and_then(build_key) else {
            continue;
        };
        // Strictly newer replaces; equal keeps the earlier member.
        if winner.as_ref().is_none_or(|(_, best)| key > *best) {
            winner = Some((versioning, key));
        }
    }
    let (versioning, _) = winner?;
    Some(Metadata {
        group_id: Some(group_id.to_owned()),
        artifact_id: Some(artifact_id.to_owned()),
        version: Some(version.to_owned()),
        versioning: Some(Versioning {
            snapshot: versioning.snapshot.clone(),
            last_updated: versioning.last_updated.clone(),
            snapshot_versions: versioning.snapshot_versions.clone(),
            ..Versioning::default()
        }),
        plugins: Vec::new(),
    })
}

fn merge_plugins(level: &MetadataLevel, documents: &[Metadata]) -> Option<Metadata> {
    let mut plugins: Vec<Plugin> = Vec::new();
    for plugin in documents.iter().flat_map(|d| d.plugins.iter()) {
        let known = plugins
            .iter()
            .any(|p| p.prefix == plugin.prefix && p.artifact_id == plugin.artifact_id);
        if !known {
            plugins.push(plugin.clone());
        }
    }
    if plugins.is_empty() {
        return None;
    }
    // The directory of a group level document is the groupId with slashes.
    let group_id = level.dir().replace('/', ".");
    Some(Metadata {
        group_id: Some(group_id),
        artifact_id: None,
        version: None,
        versioning: None,
        plugins,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{SnapshotVersion, parse_metadata};

    fn artifact_level() -> MetadataLevel {
        MetadataLevel::Artifact {
            group_id: "org.acme".into(),
            artifact_id: "lib".into(),
        }
    }

    fn versions_doc(versions: &[&str], last_updated: &str) -> Metadata {
        Metadata {
            group_id: Some("org.acme".into()),
            artifact_id: Some("lib".into()),
            versioning: Some(Versioning {
                latest: Some("9.9".into()),
                release: Some("9.9".into()),
                versions: versions.iter().map(|v| (*v).to_owned()).collect(),
                last_updated: Some(last_updated.into()),
                ..Versioning::default()
            }),
            ..Metadata::default()
        }
    }

    #[test]
    fn unions_versions_and_recomputes_latest_and_release() {
        let merged = merge_documents(
            &artifact_level(),
            &[
                versions_doc(&["1.0", "1.1"], "20240101000000"),
                versions_doc(&["1.1", "2.0-SNAPSHOT", "1.2"], "20240301000000"),
            ],
        )
        .unwrap();
        let v = merged.versioning.unwrap();
        assert_eq!(v.versions, ["1.0", "1.1", "1.2", "2.0-SNAPSHOT"]);
        assert_eq!(v.latest.as_deref(), Some("2.0-SNAPSHOT"));
        assert_eq!(v.release.as_deref(), Some("1.2"));
        assert_eq!(v.last_updated.as_deref(), Some("20240301000000"));
        assert_eq!(merged.group_id.as_deref(), Some("org.acme"));
    }

    #[test]
    fn a_member_cannot_rename_the_artifact_it_answers_for() {
        let mut doc = versions_doc(&["1.0"], "20240101000000");
        doc.artifact_id = Some("evil".into());
        doc.group_id = Some("evil.group".into());
        let merged = merge_documents(&artifact_level(), &[doc]).unwrap();
        assert_eq!(merged.artifact_id.as_deref(), Some("lib"));
        assert_eq!(merged.group_id.as_deref(), Some("org.acme"));
    }

    #[test]
    fn documents_without_versions_give_nothing() {
        assert!(merge_documents(&artifact_level(), &[Metadata::default()]).is_none());
        assert!(merge_documents(&artifact_level(), &[]).is_none());
    }

    fn snapshot_doc(timestamp: &str, build: u32, extensions: &[&str]) -> Metadata {
        Metadata {
            version: Some("1.0-SNAPSHOT".into()),
            versioning: Some(Versioning {
                snapshot: Some(Snapshot {
                    timestamp: Some(timestamp.into()),
                    build_number: Some(build),
                }),
                last_updated: Some(timestamp.replace('.', "")),
                snapshot_versions: extensions
                    .iter()
                    .map(|e| SnapshotVersion {
                        classifier: None,
                        extension: (*e).to_owned(),
                        value: format!("1.0-{timestamp}-{build}"),
                        updated: None,
                    })
                    .collect(),
                ..Versioning::default()
            }),
            ..Metadata::default()
        }
    }

    fn version_level() -> MetadataLevel {
        MetadataLevel::Version {
            group_id: "org.acme".into(),
            artifact_id: "lib".into(),
            version: "1.0-SNAPSHOT".into(),
        }
    }

    #[test]
    fn the_newest_snapshot_build_wins_as_a_whole() {
        let merged = merge_documents(
            &version_level(),
            &[
                snapshot_doc("20240102.030405", 3, &["jar", "pom", "module"]),
                snapshot_doc("20240103.030405", 1, &["jar"]),
            ],
        )
        .unwrap();
        let v = merged.versioning.unwrap();
        assert_eq!(
            v.snapshot.unwrap().timestamp.as_deref(),
            Some("20240103.030405")
        );
        assert_eq!(
            v.snapshot_versions.len(),
            1,
            "entries of two members are not mixed"
        );
    }

    #[test]
    fn a_tie_goes_to_the_earlier_member() {
        let merged = merge_documents(
            &version_level(),
            &[
                snapshot_doc("20240102.030405", 3, &["jar"]),
                snapshot_doc("20240102.030405", 3, &["jar", "pom"]),
            ],
        )
        .unwrap();
        assert_eq!(merged.versioning.unwrap().snapshot_versions.len(), 1);
    }

    #[test]
    fn a_document_for_another_version_does_not_answer() {
        let mut other = snapshot_doc("20250102.030405", 3, &["jar"]);
        other.version = Some("2.0-SNAPSHOT".into());
        let merged = merge_documents(&version_level(), &[other]);
        assert!(merged.is_none());
    }

    #[test]
    fn plugin_prefixes_are_unioned_first_member_wins() {
        let a = parse_metadata(
            b"<metadata><plugins><plugin><name>A</name><prefix>p</prefix><artifactId>p-plugin</artifactId></plugin></plugins></metadata>",
        )
        .unwrap();
        let b = parse_metadata(
            b"<metadata><plugins><plugin><name>B</name><prefix>p</prefix><artifactId>p-plugin</artifactId></plugin>\
              <plugin><prefix>q</prefix><artifactId>q-plugin</artifactId></plugin></plugins></metadata>",
        )
        .unwrap();
        let level = MetadataLevel::Artifact {
            group_id: "org.apache.maven".into(),
            artifact_id: "plugins".into(),
        };
        let merged = merge_documents(&level, &[a, b]).unwrap();
        assert_eq!(merged.group_id.as_deref(), Some("org.apache.maven.plugins"));
        assert_eq!(merged.plugins.len(), 2);
        assert_eq!(merged.plugins[0].name.as_deref(), Some("A"));
    }

    #[test]
    fn a_group_level_path_without_plugins_gives_nothing() {
        let level = MetadataLevel::Group {
            group_id: "junit".into(),
        };
        assert!(merge_documents(&level, &[versions_doc(&["1"], "20240101000000")]).is_none());
    }
}
