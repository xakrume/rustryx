//! Chart names, versions and the file names built from them.
//!
//! A hosted chart is stored as `charts/<name>-<version>.tgz`. The rules that make that name
//! unambiguous are what lets a request path be turned back into a chart name (for the
//! dependency confusion claim of a group) without reading anything:
//!
//! * a **name** is a DNS-1123 label: lowercase letters, digits and `-`, starting and ending
//!   with a letter or digit, at most 63 characters. It has no dot, so the dotted part of a
//!   semantic version can never start inside a name;
//! * a **version** is a strict `SemVer` 2.0 version in its canonical spelling (no leading `v`, no
//!   missing component, no leading zeros), so the text in the file name is the text in
//!   `Chart.yaml`.
//!
//! Together: in `<name>-<version>` at most one `-` splits the text into a valid name and a
//! valid version, because a valid version starts with `<digits>.<digits>.<digits>` and any
//! later `-` would put those dots into the name.
//!
//! This is stricter than Helm itself (`helm lint` accepts uppercase and underscores). The
//! stricter rule is a deliberate deviation: it removes look-alike names (`Redis` and `redis`)
//! and the ambiguity above, and every chart name in common use already follows it.

use semver::Version;

/// Longest chart name, the DNS label limit.
pub const MAX_NAME_LEN: usize = 63;

/// Why a name or version was refused. The text is client-safe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// The chart name breaks the DNS label rule.
    #[error("the chart name must be lowercase letters, digits and dashes, at most 63 characters")]
    BadName,
    /// The chart version is not a canonical `SemVer` 2.0 version.
    #[error("the chart version must be a semantic version such as 1.2.3")]
    BadVersion,
}

/// True for a DNS-1123 label.
#[must_use]
pub fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_LEN {
        return false;
    }
    let edge = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    edge(bytes[0]) && edge(bytes[bytes.len() - 1]) && bytes.iter().all(|b| edge(*b) || *b == b'-')
}

/// Parses a version and requires its canonical spelling.
pub fn parse_version(text: &str) -> Result<Version, NameError> {
    let version = Version::parse(text).map_err(|_| NameError::BadVersion)?;
    if version.to_string() == text {
        Ok(version)
    } else {
        Err(NameError::BadVersion)
    }
}

/// Validates a name and a version together.
pub fn validate(name: &str, version: &str) -> Result<Version, NameError> {
    if !is_valid_name(name) {
        return Err(NameError::BadName);
    }
    parse_version(version)
}

/// `<name>-<version>.tgz`
#[must_use]
pub fn archive_file_name(name: &str, version: &str) -> String {
    format!("{name}-{version}.tgz")
}

/// A chart file name taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartFile {
    /// The chart name.
    pub name: String,
    /// The chart version, canonical.
    pub version: String,
    /// True for the provenance file (`.tgz.prov`), false for the archive.
    pub provenance: bool,
}

impl ChartFile {
    /// The file name as it is stored and requested.
    #[must_use]
    pub fn file_name(&self) -> String {
        let archive = archive_file_name(&self.name, &self.version);
        if self.provenance {
            format!("{archive}.prov")
        } else {
            archive
        }
    }
}

/// Splits `<name>-<version>.tgz` or `<name>-<version>.tgz.prov` into its parts, or `None`
/// when the text is not such a name. See the module docs for why the split is unique.
#[must_use]
pub fn split_chart_file(file: &str) -> Option<ChartFile> {
    let (stem, provenance) = match file.strip_suffix(".tgz.prov") {
        Some(stem) => (stem, true),
        None => (file.strip_suffix(".tgz")?, false),
    };
    stem.match_indices('-').find_map(|(at, _)| {
        let (name, version) = (&stem[..at], &stem[at + 1..]);
        (is_valid_name(name) && parse_version(version).is_ok()).then(|| ChartFile {
            name: name.to_owned(),
            version: version.to_owned(),
            provenance,
        })
    })
}

/// A version as a comparable value for sorting, for text that may not be strict `SemVer`: a
/// public repository can carry `v1.2.3` or `1.2`. A leading `v` is dropped and missing minor
/// and patch components are zero. `None` means the text is not a version at all.
#[must_use]
pub fn lenient_version(text: &str) -> Option<Version> {
    if let Ok(version) = Version::parse(text) {
        return Some(version);
    }
    let trimmed = text.trim().trim_start_matches(['v', 'V']);
    let (core, rest) = match trimmed.find(['-', '+']) {
        Some(at) => trimmed.split_at(at),
        None => (trimmed, ""),
    };
    let mut parts: Vec<&str> = core.split('.').collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    while parts.len() < 3 {
        parts.push("0");
    }
    Version::parse(&format!("{}{rest}", parts.join("."))).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn names_follow_the_dns_label_rule() {
        for good in ["a", "web", "my-chart", "chart-2", "9lives", &"a".repeat(63)] {
            assert!(is_valid_name(good), "{good}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "A",
            "My-Chart",
            "my_chart",
            "my.chart",
            "my chart",
            "a/b",
            "..",
            "é",
            &"a".repeat(64),
        ] {
            assert!(!is_valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn versions_must_be_canonical_semver() {
        for good in [
            "0.0.1",
            "1.2.3",
            "1.2.3-rc.1",
            "1.2.3+build.5",
            "1.2.3-alpha-1+b",
        ] {
            assert!(parse_version(good).is_ok(), "{good}");
        }
        for bad in [
            "1", "1.2", "v1.2.3", "01.2.3", "1.2.3.4", "1.2.3-", "", " 1.2.3", "1.2.3 ", "latest",
        ] {
            assert_eq!(parse_version(bad), Err(NameError::BadVersion), "{bad:?}");
        }
    }

    #[test]
    fn validate_reports_the_name_before_the_version() {
        assert_eq!(validate("Bad", "nope"), Err(NameError::BadName));
        assert_eq!(validate("good", "nope"), Err(NameError::BadVersion));
        assert!(validate("good", "1.0.0").is_ok());
        assert!(NameError::BadName.to_string().contains("lowercase"));
        assert!(
            NameError::BadVersion
                .to_string()
                .contains("semantic version")
        );
    }

    #[test]
    fn splits_a_file_name_into_name_and_version() {
        let cases = [
            ("web-1.2.3.tgz", "web", "1.2.3", false),
            ("my-chart-1.2.3-rc.1.tgz", "my-chart", "1.2.3-rc.1", false),
            ("a-b-c-0.1.0+build.5.tgz", "a-b-c", "0.1.0+build.5", false),
            ("foo-1.0.0-1.0.0.tgz", "foo", "1.0.0-1.0.0", false),
            ("web-1.2.3.tgz.prov", "web", "1.2.3", true),
            ("chart-2-0.1.0.tgz", "chart-2", "0.1.0", false),
        ];
        for (file, name, version, provenance) in cases {
            let parsed = split_chart_file(file).unwrap_or_else(|| panic!("{file}"));
            assert_eq!(
                (
                    parsed.name.as_str(),
                    parsed.version.as_str(),
                    parsed.provenance
                ),
                (name, version, provenance),
                "{file}"
            );
            assert_eq!(parsed.file_name(), file);
        }
    }

    #[test]
    fn other_names_are_not_chart_files() {
        for file in [
            "web.tgz",
            "web-1.2.tgz",
            "web-v1.2.3.tgz",
            "Web-1.2.3.tgz",
            "web-1.2.3.zip",
            "web-1.2.3.tgz.sig",
            "-1.2.3.tgz",
            "web-.tgz",
            "my.chart-1.2.3.tgz",
            ".tgz",
            "",
        ] {
            assert!(split_chart_file(file).is_none(), "{file:?}");
        }
    }

    #[test]
    fn lenient_versions_cover_what_public_repositories_publish() {
        for (text, expected) in [
            ("1.2.3", "1.2.3"),
            ("v1.2.3", "1.2.3"),
            ("1.2", "1.2.0"),
            ("1", "1.0.0"),
            ("v2", "2.0.0"),
            ("1.2-rc.1", "1.2.0-rc.1"),
            ("1.2.3+meta", "1.2.3+meta"),
        ] {
            assert_eq!(
                lenient_version(text).map(|v| v.to_string()).as_deref(),
                Some(expected),
                "{text}"
            );
        }
        for text in ["", "latest", "a.b.c", "1..2", "1.2.3.4", "-"] {
            assert!(lenient_version(text).is_none(), "{text:?}");
        }
    }

    proptest! {
        #[test]
        fn the_split_of_a_built_name_always_returns_the_parts(
            name in "[a-z0-9]([a-z0-9-]{0,20}[a-z0-9])?",
            major in 0u64..1000, minor in 0u64..1000, patch in 0u64..1000,
            pre in proptest::option::of("[a-z][a-z0-9-]{0,6}(\\.[0-9]{1,3})?"),
        ) {
            let version = match &pre {
                Some(pre) => format!("{major}.{minor}.{patch}-{pre}"),
                None => format!("{major}.{minor}.{patch}"),
            };
            prop_assume!(parse_version(&version).is_ok());
            let file = archive_file_name(&name, &version);
            let parsed = split_chart_file(&file).expect("a built name splits");
            prop_assert_eq!(parsed.name, name);
            prop_assert_eq!(parsed.version, version);
            prop_assert!(!parsed.provenance);
        }

        #[test]
        fn splitting_never_panics(file in ".{0,120}") {
            if let Some(parsed) = split_chart_file(&file) {
                prop_assert_eq!(parsed.file_name(), file.as_str());
            }
            let _ = lenient_version(&file);
        }
    }
}
