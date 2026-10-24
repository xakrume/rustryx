//! Paths of a classic Helm chart repository.
//!
//! Two addresses exist: `index.yaml`, and `charts/<name>-<version>.tgz`. A hosted repository
//! accepts only a file name that [`crate::names::split_chart_file`] can take apart (DNS label,
//! canonical `SemVer`). A proxy or a group also accepts any other single `charts/*.tgz` segment,
//! because an upstream chart is not required to follow that spelling. The object is still stored
//! and fetched under the path the client named: authorization saw that path.

use rustryx_core::model::RepoType;
use rustryx_format_core::{FormatError, NormalizedPath};

use crate::names::{ChartFile, split_chart_file};

/// Client text when a hosted path is not `charts/<name>-<version>.tgz`.
pub(crate) const BAD_CHART_FILE: &str =
    "the chart file name must be a DNS label, a dash, and a semantic version, ending in .tgz";

const NOT_A_HELM_PATH: &str = "the path is not a helm chart repository path";

/// What a normalized path addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// Generated or proxied `index.yaml`.
    Index,
    /// `charts/<name>-<version>.tgz` with a DNS label and a canonical version.
    Chart(ChartFile),
    /// `charts/<file>.tgz` that a proxy may forward but a hosted repository does not store.
    LooseChart(String),
}

/// Classifies `path` for `repo_type`.
///
/// # Errors
/// [`FormatError::BadRequest`] when the path is not a helm path, or (on a hosted repository)
/// when the chart file name is not a DNS label plus a canonical version.
pub(crate) fn classify_path(
    path: &NormalizedPath,
    repo_type: RepoType,
) -> Result<Target, FormatError> {
    if path.has_trailing_slash() {
        return Err(FormatError::bad_request(NOT_A_HELM_PATH));
    }
    match interpret(path.as_str()) {
        Some(Target::LooseChart(_)) if repo_type == RepoType::Hosted => {
            Err(FormatError::bad_request(BAD_CHART_FILE))
        }
        Some(target) => Ok(target),
        None => Err(FormatError::bad_request(NOT_A_HELM_PATH)),
    }
}

/// The same classification without the hosted restriction. Group members are called with a
/// request the group already accepted, including a loose chart file name.
pub(crate) fn interpret(path: &str) -> Option<Target> {
    if path.is_empty() || path.ends_with('/') {
        return None;
    }
    if path == "index.yaml" {
        return Some(Target::Index);
    }
    let file = path.strip_prefix("charts/")?;
    if file.contains('/') {
        return None;
    }
    if let Some(parsed) = split_chart_file(file)
        && !parsed.provenance
    {
        return Some(Target::Chart(parsed));
    }
    is_safe_chart_filename(file).then(|| Target::LooseChart(file.to_owned()))
}

/// A single archive file name that cannot leave `charts/`.
pub(crate) fn is_safe_chart_filename(name: &str) -> bool {
    let bytes = name.as_bytes();
    let tgz = std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tgz"));
    !bytes.is_empty()
        && bytes.len() <= 255
        && tgz
        && !name.contains(['/', '\\'])
        && !name.contains("..")
        && bytes.iter().all(|byte| !byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(raw: &str) -> NormalizedPath {
        NormalizedPath::parse(raw).unwrap()
    }

    #[test]
    fn index_and_a_canonical_chart_are_accepted() {
        assert_eq!(
            classify_path(&path("index.yaml"), RepoType::Hosted).unwrap(),
            Target::Index
        );
        match classify_path(&path("charts/web-1.2.3.tgz"), RepoType::Hosted).unwrap() {
            Target::Chart(file) => {
                assert_eq!(file.name, "web");
                assert_eq!(file.version, "1.2.3");
                assert!(!file.provenance);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_hosted_chart_name_that_is_not_a_dns_label_is_refused() {
        for raw in [
            "charts/Web-1.0.0.tgz",
            "charts/web-1.2.tgz",
            "charts/upload.tgz",
        ] {
            match classify_path(&path(raw), RepoType::Hosted) {
                Err(FormatError::BadRequest(message)) => {
                    assert!(message.contains("DNS label"), "{raw}: {message}");
                }
                other => panic!("{raw}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_proxy_may_name_a_chart_file_the_hosted_rule_refuses() {
        match classify_path(&path("charts/Web-1.0.0.tgz"), RepoType::Proxy).unwrap() {
            Target::LooseChart(file) => assert_eq!(file, "Web-1.0.0.tgz"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn other_paths_are_not_helm_paths() {
        for raw in [
            "",
            "charts",
            "charts/",
            "index.yml",
            "charts/a/web-1.0.0.tgz",
            "readme.txt",
        ] {
            let Ok(normalized) = NormalizedPath::parse(raw) else {
                continue;
            };
            assert!(
                classify_path(&normalized, RepoType::Hosted).is_err(),
                "{raw}"
            );
        }
    }

    #[test]
    fn traversal_never_becomes_a_path() {
        for raw in [
            "../index.yaml",
            "charts/../index.yaml",
            "charts/%2e%2e/index.yaml",
            "a/../../x",
        ] {
            assert!(
                matches!(
                    NormalizedPath::parse(raw),
                    Err(rustryx_format_core::PathError::Traversal
                        | rustryx_format_core::PathError::DoubleEncoded)
                ),
                "{raw}"
            );
            assert!(matches!(
                FormatError::from(rustryx_format_core::PathError::Traversal),
                FormatError::BadRequest(_)
            ));
        }
    }
}
