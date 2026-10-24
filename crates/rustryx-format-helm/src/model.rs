//! The chart metadata that appears both in a `Chart.yaml` and in an `index.yaml` entry.
//!
//! Helm's index entry is the chart's metadata plus where to download it (`urls`), when it was
//! added (`created`) and the SHA-256 of the archive (`digest`). One struct serves both
//! documents: parsing a `Chart.yaml` leaves the three index fields empty.
//!
//! Every field is optional on input except where validation says otherwise: public
//! repositories publish indexes with missing and oddly typed fields, and one odd entry must
//! not make the whole repository unreadable. Scalars are read as text whatever their YAML
//! type (`appVersion: 1.16` is the text `1.16`). Keys this struct does not know are dropped,
//! which means an index that went through Rustryx (a proxy rewrite or a group merge) keeps
//! the fields Helm and the Helm tools read and loses private extensions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A maintainer of a chart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maintainer {
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// E-mail address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Web address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A dependency of a chart (`apiVersion: v2`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    /// Chart name.
    #[serde(default)]
    pub name: String,
    /// Version or version range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Repository URL or `@alias`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Condition path in the values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Tags that enable the dependency.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Whether it is enabled by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Values to import from the dependency.
    #[serde(
        default,
        rename = "import-values",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub import_values: Vec<serde_json::Value>,
    /// Alias used instead of the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

/// One version of one chart: `Chart.yaml` plus the index fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartVersion {
    /// Chart name.
    #[serde(default)]
    pub name: String,
    /// Chart version.
    #[serde(default)]
    pub version: String,
    /// `v1` or `v2`.
    #[serde(
        default,
        rename = "apiVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub api_version: Option<String>,
    /// Version of the application the chart deploys.
    #[serde(
        default,
        rename = "appVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub app_version: Option<String>,
    /// One line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `application` or `library`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Project home page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// Icon URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Search keywords.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Source URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Maintainers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maintainers: Vec<Maintainer>,
    /// Dependencies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<Dependency>,
    /// Free form annotations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    /// Kubernetes version constraint.
    #[serde(
        default,
        rename = "kubeVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub kube_version: Option<String>,
    /// Whether the chart is deprecated.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deprecated: bool,
    /// Legacy `v1` dependency condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Legacy `v1` dependency tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<String>,
    /// Where the archive can be downloaded (index entries).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// When the version was added, RFC 3339 (index entries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Whether the version was removed from the repository (index entries).
    #[serde(default, skip_serializing_if = "is_false")]
    pub removed: bool,
    /// SHA-256 of the archive, lowercase hex (index entries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// An `index.yaml` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexFile {
    /// Always `v1`.
    #[serde(default = "default_api_version", rename = "apiVersion")]
    pub api_version: String,
    /// Versions per chart name. `entries:` with no value is an empty map.
    #[serde(default, deserialize_with = "null_as_default")]
    pub entries: BTreeMap<String, Vec<ChartVersion>>,
    /// When the index was generated, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
}

fn default_api_version() -> String {
    "v1".to_owned()
}

/// A YAML `key:` with no value is `null`, which Helm reads as an empty map.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

impl Default for IndexFile {
    fn default() -> Self {
        Self {
            api_version: default_api_version(),
            entries: BTreeMap::new(),
            generated: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml::{YamlLimits, parse, render};

    #[test]
    fn an_entry_round_trips_through_yaml_with_the_helm_field_names() {
        let entry = ChartVersion {
            name: "web".into(),
            version: "1.2.3".into(),
            api_version: Some("v2".into()),
            app_version: Some("1.16".into()),
            description: Some("A web chart".into()),
            kind: Some("application".into()),
            keywords: vec!["web".into()],
            maintainers: vec![Maintainer {
                name: Some("Ada".into()),
                email: Some("ada@example.org".into()),
                url: None,
            }],
            dependencies: vec![Dependency {
                name: "db".into(),
                version: Some("~1.0".into()),
                repository: Some("https://charts.example.org".into()),
                import_values: vec![serde_json::json!({"child": "a", "parent": "b"})],
                ..Dependency::default()
            }],
            annotations: BTreeMap::from([("k".into(), "v".into())]),
            kube_version: Some(">=1.25.0-0".into()),
            urls: vec!["https://r.example/charts/web-1.2.3.tgz".into()],
            created: Some("2026-10-24T10:00:00Z".into()),
            digest: Some("ab".repeat(32)),
            ..ChartVersion::default()
        };
        let index = IndexFile {
            entries: BTreeMap::from([("web".into(), vec![entry])]),
            generated: Some("2026-10-24T10:00:00Z".into()),
            ..IndexFile::default()
        };
        let text = render(&index).unwrap();
        for key in [
            "apiVersion:",
            "appVersion:",
            "kubeVersion:",
            "import-values:",
            "entries:",
            "digest:",
        ] {
            assert!(text.contains(key), "{key} in {text}");
        }
        assert!(
            !text.contains("deprecated"),
            "false flags are not written: {text}"
        );
        let back: IndexFile = parse(text.as_bytes(), YamlLimits::index(1 << 20)).unwrap();
        assert_eq!(back, index);
    }

    #[test]
    fn a_helm_written_index_is_read() {
        let text = "apiVersion: v1\n\
entries:\n  nginx:\n  - apiVersion: v2\n    appVersion: 1.25.3\n    created: \"2023-11-01T10:00:00.123456789Z\"\n    \
description: NGINX\n    digest: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n    \
home: https://nginx.org\n    keywords:\n    - web\n    - proxy\n    name: nginx\n    \
type: application\n    urls:\n    - https://example.org/charts/nginx-1.0.0.tgz\n    version: 1.0.0\n    deprecated: true\n\
generated: \"2023-11-01T10:00:00Z\"\n";
        let index: IndexFile = parse(text.as_bytes(), YamlLimits::index(1 << 20)).unwrap();
        let versions = &index.entries["nginx"];
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].app_version.as_deref(), Some("1.25.3"));
        assert!(versions[0].deprecated);
        assert_eq!(versions[0].keywords, ["web", "proxy"]);
        assert_eq!(index.generated.as_deref(), Some("2023-11-01T10:00:00Z"));
    }

    #[test]
    fn missing_and_null_parts_are_empty_not_errors() {
        let empty: IndexFile =
            parse(b"apiVersion: v1\nentries:\n", YamlLimits::index(4096)).unwrap();
        assert!(empty.entries.is_empty());
        let none: IndexFile = parse(b"apiVersion: v1\n", YamlLimits::index(4096)).unwrap();
        assert!(none.entries.is_empty());
        let bare: IndexFile = parse(b"entries: {}\n", YamlLimits::index(4096)).unwrap();
        assert_eq!(bare.api_version, "v1");
        let odd: IndexFile = parse(
            b"entries:\n  a:\n  - name: a\n    version: 1.0\n    appVersion: ~\n    description:\n",
            YamlLimits::index(4096),
        )
        .unwrap();
        assert_eq!(odd.entries["a"][0].version, "1.0");
        assert_eq!(odd.entries["a"][0].app_version, None);
    }
}
