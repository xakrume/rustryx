//! Versions and dist-tag names.
//!
//! Versions are strict `SemVer` 2.0 (<https://semver.org>), which is what node-semver calls a valid
//! version. The loose spellings node-semver can clean (`v1.2.3`, `=1.2.3`, `1.2`) are refused: the
//! npm client cleans them before it publishes, and a registry that stored them would have two
//! spellings of one release.

use std::cmp::Ordering;

use semver::{Version, VersionReq};

/// Longest version string accepted (the limit node-semver applies).
pub const MAX_VERSION_LEN: usize = 256;

/// Longest dist-tag name accepted.
pub const MAX_TAG_LEN: usize = 128;

/// Why a version or a tag was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    /// Not strict `SemVer`, or longer than [`MAX_VERSION_LEN`].
    #[error("not a valid semantic version")]
    Invalid,
    /// A dist-tag name that is empty, too long, uses forbidden characters or looks like a
    /// version or a version range (npm refuses those so `npm install pkg@tag` stays unambiguous).
    #[error("not a valid dist-tag name")]
    InvalidTag,
}

/// Parses a strict `SemVer` version.
///
/// # Errors
/// [`VersionError::Invalid`] for anything that is not exactly `SemVer` 2.0.
pub fn parse_version(text: &str) -> Result<Version, VersionError> {
    if text.is_empty() || text.len() > MAX_VERSION_LEN || text.trim() != text {
        return Err(VersionError::Invalid);
    }
    Version::parse(text).map_err(|_| VersionError::Invalid)
}

/// Validates a dist-tag name.
///
/// # Errors
/// [`VersionError::InvalidTag`] when the name cannot be a tag.
pub fn validate_tag(tag: &str) -> Result<(), VersionError> {
    let well_formed = !tag.is_empty()
        && tag.len() <= MAX_TAG_LEN
        && tag.starts_with(|c: char| c.is_ascii_alphanumeric())
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+'));
    if !well_formed {
        return Err(VersionError::InvalidTag);
    }
    // `npm install pkg@1.x` must mean a range, never a tag named `1.x`.
    if Version::parse(tag).is_ok() || VersionReq::parse(tag).is_ok() {
        return Err(VersionError::InvalidTag);
    }
    Ok(())
}

/// True when both versions are the same release: `SemVer` says build metadata does not take part in
/// precedence, so `1.0.0+a` and `1.0.0+b` are one version and the second cannot be published.
#[must_use]
pub fn same_release(a: &Version, b: &Version) -> bool {
    a.cmp_precedence(b) == Ordering::Equal
}

/// The highest version by `SemVer` precedence, preferring releases over prereleases: `latest`
/// should not jump to `2.0.0-rc.1` while `1.9.0` is the newest release.
#[must_use]
pub fn highest<'a, I>(versions: I) -> Option<&'a Version>
where
    I: IntoIterator<Item = &'a Version>,
{
    let mut best_stable: Option<&Version> = None;
    let mut best_any: Option<&Version> = None;
    for version in versions {
        if best_any.is_none_or(|b| version.cmp_precedence(b) == Ordering::Greater) {
            best_any = Some(version);
        }
        if version.pre.is_empty()
            && best_stable.is_none_or(|b| version.cmp_precedence(b) == Ordering::Greater)
        {
            best_stable = Some(version);
        }
    }
    best_stable.or(best_any)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_semver_only() {
        for good in [
            "0.0.0",
            "1.2.3",
            "1.0.0-rc.1",
            "1.0.0+build.5",
            "10.20.30-a.b+c",
        ] {
            assert!(parse_version(good).is_ok(), "{good}");
        }
        for bad in [
            "", "1", "1.2", "v1.2.3", "=1.2.3", " 1.2.3", "1.2.3 ", "01.2.3", "1.2.3.4", "latest",
            "1.2.3-", "1.2.x", "^1.2.3", "1.2.3\n",
        ] {
            assert!(parse_version(bad).is_err(), "{bad:?}");
        }
        assert!(parse_version(&format!("1.0.0-{}", "a".repeat(MAX_VERSION_LEN))).is_err());
    }

    #[test]
    fn tag_rules() {
        for good in ["latest", "next", "beta-2", "rc_1", "stable.x86", "canary"] {
            assert!(validate_tag(good).is_ok(), "{good}");
        }
        for bad in [
            "", "1.0.0", "1.x", "*", ">=1", "^1.0.0", "~1.0.0", "-x", ".x", "a b", "a/b", "a%2fb",
            "ü",
        ] {
            assert!(validate_tag(bad).is_err(), "{bad:?}");
        }
        assert!(validate_tag(&"a".repeat(MAX_TAG_LEN + 1)).is_err());
    }

    #[test]
    fn build_metadata_does_not_make_a_new_release() {
        let a = Version::parse("1.0.0+a").unwrap();
        let b = Version::parse("1.0.0+b").unwrap();
        let c = Version::parse("1.0.1").unwrap();
        assert!(same_release(&a, &b));
        assert!(!same_release(&a, &c));
    }

    #[test]
    fn highest_prefers_releases() {
        let versions: Vec<Version> = ["1.0.0", "1.9.0", "2.0.0-rc.1", "1.10.0"]
            .iter()
            .map(|v| Version::parse(v).unwrap())
            .collect();
        assert_eq!(highest(&versions).unwrap().to_string(), "1.10.0");
        let only_pre: Vec<Version> = ["1.0.0-a", "1.0.0-b"]
            .iter()
            .map(|v| Version::parse(v).unwrap())
            .collect();
        assert_eq!(highest(&only_pre).unwrap().to_string(), "1.0.0-b");
        assert!(highest(&[]).is_none());
    }
}
