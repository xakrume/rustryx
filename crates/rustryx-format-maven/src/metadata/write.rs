//! Canonical writer of `maven-metadata.xml`.

use std::fmt::Write as _;

use super::{Metadata, Plugin, Snapshot, SnapshotVersion, Versioning};

/// Writes the document in one canonical form: fixed element order, two space indent, `\n`
/// line ends, a trailing newline. Equal models give equal bytes, which is what lets a
/// generated document have a stable `ETag` and lets the checksum files be derived from it.
///
/// `modelVersion="1.1.0"` is written only when `snapshotVersions` is, because that is the
/// element that version of the model introduced.
#[must_use]
pub fn write_metadata(metadata: &Metadata) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let has_snapshot_versions = metadata
        .versioning
        .as_ref()
        .is_some_and(|v| !v.snapshot_versions.is_empty());
    if has_snapshot_versions {
        out.push_str("<metadata modelVersion=\"1.1.0\">\n");
    } else {
        out.push_str("<metadata>\n");
    }
    element(&mut out, 1, "groupId", metadata.group_id.as_deref());
    element(&mut out, 1, "artifactId", metadata.artifact_id.as_deref());
    element(&mut out, 1, "version", metadata.version.as_deref());
    if let Some(versioning) = &metadata.versioning {
        write_versioning(&mut out, versioning);
    }
    if !metadata.plugins.is_empty() {
        out.push_str("  <plugins>\n");
        for plugin in &metadata.plugins {
            write_plugin(&mut out, plugin);
        }
        out.push_str("  </plugins>\n");
    }
    out.push_str("</metadata>\n");
    out
}

fn write_versioning(out: &mut String, versioning: &Versioning) {
    out.push_str("  <versioning>\n");
    element(out, 2, "latest", versioning.latest.as_deref());
    element(out, 2, "release", versioning.release.as_deref());
    if let Some(snapshot) = &versioning.snapshot {
        write_snapshot(out, snapshot);
    }
    if !versioning.versions.is_empty() {
        out.push_str("    <versions>\n");
        for version in &versioning.versions {
            element(out, 3, "version", Some(version));
        }
        out.push_str("    </versions>\n");
    }
    element(out, 2, "lastUpdated", versioning.last_updated.as_deref());
    if !versioning.snapshot_versions.is_empty() {
        out.push_str("    <snapshotVersions>\n");
        for entry in &versioning.snapshot_versions {
            write_snapshot_version(out, entry);
        }
        out.push_str("    </snapshotVersions>\n");
    }
    out.push_str("  </versioning>\n");
}

fn write_snapshot(out: &mut String, snapshot: &Snapshot) {
    out.push_str("    <snapshot>\n");
    element(out, 3, "timestamp", snapshot.timestamp.as_deref());
    if let Some(build) = snapshot.build_number {
        element(out, 3, "buildNumber", Some(&build.to_string()));
    }
    out.push_str("    </snapshot>\n");
}

fn write_snapshot_version(out: &mut String, entry: &SnapshotVersion) {
    out.push_str("      <snapshotVersion>\n");
    element(out, 4, "classifier", entry.classifier.as_deref());
    element(out, 4, "extension", Some(&entry.extension));
    element(out, 4, "value", Some(&entry.value));
    element(out, 4, "updated", entry.updated.as_deref());
    out.push_str("      </snapshotVersion>\n");
}

fn write_plugin(out: &mut String, plugin: &Plugin) {
    out.push_str("    <plugin>\n");
    element(out, 3, "name", plugin.name.as_deref());
    element(out, 3, "prefix", Some(&plugin.prefix));
    element(out, 3, "artifactId", Some(&plugin.artifact_id));
    out.push_str("    </plugin>\n");
}

fn element(out: &mut String, depth: usize, name: &str, value: Option<&str>) {
    let Some(value) = value else { return };
    for _ in 0..depth {
        out.push_str("  ");
    }
    let _ = write!(out, "<{name}>");
    escape_into(out, value);
    let _ = writeln!(out, "</{name}>");
}

fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::parse_metadata;
    use proptest::prelude::*;

    #[test]
    fn writes_an_artifact_level_document() {
        let metadata = Metadata {
            group_id: Some("org.acme".into()),
            artifact_id: Some("lib".into()),
            versioning: Some(Versioning {
                latest: Some("1.1".into()),
                release: Some("1.1".into()),
                versions: vec!["1.0".into(), "1.1".into()],
                last_updated: Some("20240102030405".into()),
                ..Versioning::default()
            }),
            ..Metadata::default()
        };
        let text = write_metadata(&metadata);
        assert_eq!(
            text,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<metadata>\n  <groupId>org.acme</groupId>\n  \
             <artifactId>lib</artifactId>\n  <versioning>\n    <latest>1.1</latest>\n    \
             <release>1.1</release>\n    <versions>\n      <version>1.0</version>\n      \
             <version>1.1</version>\n    </versions>\n    <lastUpdated>20240102030405</lastUpdated>\n  \
             </versioning>\n</metadata>\n"
        );
    }

    #[test]
    fn escapes_text() {
        let metadata = Metadata {
            plugins: vec![Plugin {
                name: Some("a<b&c>\"d'".into()),
                prefix: "p".into(),
                artifact_id: "x".into(),
            }],
            ..Metadata::default()
        };
        let text = write_metadata(&metadata);
        assert!(text.contains("a&lt;b&amp;c&gt;&quot;d&apos;"));
        assert_eq!(parse_metadata(text.as_bytes()).unwrap(), metadata);
    }

    #[test]
    fn model_version_is_written_with_snapshot_versions_only() {
        let mut metadata = Metadata::default();
        assert!(write_metadata(&metadata).contains("<metadata>"));
        metadata.versioning = Some(Versioning {
            snapshot_versions: vec![SnapshotVersion {
                classifier: None,
                extension: "jar".into(),
                value: "1.0-20240102.030405-1".into(),
                updated: None,
            }],
            ..Versioning::default()
        });
        assert!(write_metadata(&metadata).contains("modelVersion=\"1.1.0\""));
    }

    fn token() -> impl Strategy<Value = String> {
        "[A-Za-z0-9][A-Za-z0-9._+-]{0,12}"
    }

    fn time() -> impl Strategy<Value = Option<String>> {
        proptest::option::of("[0-9]{14}")
    }

    prop_compose! {
        fn versioning()(
            latest in proptest::option::of(token()),
            release in proptest::option::of(token()),
            versions in proptest::collection::vec(token(), 0..6),
            last_updated in time(),
            snapshot in proptest::option::of((proptest::option::of("[0-9]{8}\\.[0-9]{6}"), proptest::option::of(any::<u32>()))),
            snapshot_versions in proptest::collection::vec(
                (proptest::option::of(token()), token(), token(), time()),
                0..4
            ),
        ) -> Versioning {
            Versioning {
                latest,
                release,
                versions,
                last_updated,
                snapshot: snapshot.map(|(timestamp, build_number)| Snapshot { timestamp, build_number }),
                snapshot_versions: snapshot_versions
                    .into_iter()
                    .map(|(classifier, extension, value, updated)| SnapshotVersion {
                        classifier,
                        extension,
                        value,
                        updated,
                    })
                    .collect(),
            }
        }
    }

    prop_compose! {
        fn metadata()(
            group_id in proptest::option::of(token()),
            artifact_id in proptest::option::of(token()),
            version in proptest::option::of(token()),
            versioning in proptest::option::of(versioning()),
            plugins in proptest::collection::vec(
                (proptest::option::of("[ -~]{1,20}".prop_map(|s| s.trim().to_owned()).prop_filter("not empty", |s| !s.is_empty())), token(), token()),
                0..3
            ),
        ) -> Metadata {
            Metadata {
                group_id,
                artifact_id,
                version,
                versioning,
                plugins: plugins
                    .into_iter()
                    .map(|(name, prefix, artifact_id)| Plugin { name, prefix, artifact_id })
                    .collect(),
            }
        }
    }

    proptest! {
        #[test]
        fn write_then_parse_round_trips(model in metadata()) {
            let text = write_metadata(&model);
            let parsed = parse_metadata(text.as_bytes());
            prop_assert!(parsed.is_ok(), "{text}");
            let parsed = parsed.unwrap_or_default();
            // A `snapshot` or `versioning` element with no children is written as an empty
            // shell and read back as one, so only the canonical text is compared.
            prop_assert_eq!(write_metadata(&parsed), text);
        }

        #[test]
        fn normalization_is_idempotent(model in metadata()) {
            let once = write_metadata(&parse_metadata(write_metadata(&model).as_bytes()).unwrap_or_default());
            let twice = write_metadata(&parse_metadata(once.as_bytes()).unwrap_or_default());
            prop_assert_eq!(once, twice);
        }

        #[test]
        fn must_not_panic_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = parse_metadata(&bytes);
        }

        #[test]
        fn must_not_panic_on_xml_shaped_text(text in "[<>/a-zA-Z0-9 =\"&;!\\[\\]?-]{0,200}") {
            let _ = parse_metadata(text.as_bytes());
        }
    }
}
