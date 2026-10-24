//! Package versions.
//!
//! Pub versions are `SemVer` 2.0.0 and are parsed with the `semver` crate, which is strict: no
//! leading zeros, no `v` prefix, no empty identifiers. That is stricter than the Dart parser
//! (which tolerates leading zeros), and deliberately so: the text of an accepted version is
//! always its canonical rendering, so the version in a URL, in a component coordinate, in an
//! asset path and in a document is one string and two spellings of a version cannot be two
//! different published artifacts. A version that Dart accepts and this parser does not cannot be
//! published here, and a proxy upstream entry with such a version is dropped from the listing.

use std::fmt;
use std::str::FromStr;

use semver::Version;

/// Longest accepted version text, in bytes. The `semver` crate has no limit of its own.
pub const MAX_VERSION_LEN: usize = 128;

/// Why a version was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    /// The version is empty.
    #[error("version is empty")]
    Empty,
    /// The version is longer than [`MAX_VERSION_LEN`].
    #[error("version is longer than {MAX_VERSION_LEN} characters")]
    TooLong,
    /// The text is not a `SemVer` 2.0.0 version.
    #[error("version is not a valid semantic version")]
    Invalid,
}

/// A valid `SemVer` version. Equality and ordering are the ones of [`semver::Version`]: build
/// metadata takes part, so `1.0.0+1` and `1.0.0+2` are two versions (as they are for Dart).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PubVersion(Version);

impl PubVersion {
    /// Parses a version.
    ///
    /// # Errors
    /// The [`VersionError`] of the first rule the text breaks.
    pub fn parse(raw: &str) -> Result<Self, VersionError> {
        if raw.is_empty() {
            return Err(VersionError::Empty);
        }
        if raw.len() > MAX_VERSION_LEN {
            return Err(VersionError::TooLong);
        }
        let version = Version::parse(raw).map_err(|_| VersionError::Invalid)?;
        // The invariant the module docs promise. `semver` renders canonically, so a text that
        // parses but renders differently is something this crate does not know about: refuse it
        // rather than let two spellings through.
        if version.to_string() != raw {
            return Err(VersionError::Invalid);
        }
        Ok(Self(version))
    }

    /// True for a pre-release (`1.0.0-beta.1`).
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        !self.0.pre.is_empty()
    }

    /// The parsed version.
    #[must_use]
    pub fn as_semver(&self) -> &Version {
        &self.0
    }
}

impl fmt::Display for PubVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for PubVersion {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_semver() {
        for text in [
            "0.0.1",
            "1.2.3",
            "10.20.30",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-0.3.7",
            "1.0.0+build.5",
            "1.0.0-rc.1+build.5",
        ] {
            let parsed = PubVersion::parse(text).unwrap();
            assert_eq!(parsed.to_string(), text);
            assert_eq!(text.parse::<PubVersion>().unwrap(), parsed);
        }
    }

    #[test]
    fn rejects_what_is_not_canonical_semver() {
        assert_eq!(PubVersion::parse(""), Err(VersionError::Empty));
        for text in [
            "1",
            "1.0",
            "01.0.0",
            "1.0.0.0",
            "v1.0.0",
            " 1.0.0",
            "1.0.0 ",
            "1.0.0-",
            "1.0.0+",
            "1.0.0-01",
            "1.0.0-a..b",
            "1.0.0-é",
            "a.b.c",
            "../1.0.0",
            "1.0.0/x",
        ] {
            assert_eq!(
                PubVersion::parse(text),
                Err(VersionError::Invalid),
                "{text:?}"
            );
        }
        assert_eq!(
            PubVersion::parse(&format!("1.0.0-{}", "a".repeat(MAX_VERSION_LEN))),
            Err(VersionError::TooLong)
        );
    }

    #[test]
    fn orders_by_semver_precedence_with_build_as_tiebreaker() {
        let mut versions: Vec<PubVersion> =
            ["1.0.0", "1.0.0-beta", "2.0.0", "1.0.0-alpha", "1.0.0+1"]
                .iter()
                .map(|v| PubVersion::parse(v).unwrap())
                .collect();
        versions.sort();
        let sorted: Vec<String> = versions.iter().map(ToString::to_string).collect();
        assert_eq!(
            sorted,
            ["1.0.0-alpha", "1.0.0-beta", "1.0.0", "1.0.0+1", "2.0.0"]
        );
    }

    #[test]
    fn prerelease_flag() {
        assert!(PubVersion::parse("1.0.0-beta").unwrap().is_prerelease());
        assert!(!PubVersion::parse("1.0.0+meta").unwrap().is_prerelease());
        assert_eq!(PubVersion::parse("1.2.3").unwrap().as_semver().major, 1);
    }
}
