//! Validated names.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::DomainError;

/// Maximum length of a repository name.
pub const MAX_REPO_NAME_LEN: usize = 128;

/// Repository name that is safe to use as one URL path segment.
///
/// Rules: 1..=128 chars, first char ASCII alphanumeric, the rest ASCII
/// alphanumeric or `.`, `_`, `-`; no `..`; must not end with `.`. The same
/// rules keep the name usable as a docker repository prefix and a file name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RepoName(String);

impl RepoName {
    /// Parses and validates a name.
    ///
    /// # Errors
    /// Returns [`DomainError::Invalid`] when a rule is violated.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        if value.is_empty() || value.len() > MAX_REPO_NAME_LEN {
            return Err(DomainError::invalid(
                "repository name length must be 1..=128",
            ));
        }
        let mut chars = value.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
        let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !first_ok || !rest_ok {
            return Err(DomainError::invalid(
                "repository name must start with a letter or digit and contain only letters, digits, '.', '_' and '-'",
            ));
        }
        if value.contains("..") || value.ends_with('.') {
            return Err(DomainError::invalid(
                "repository name must not contain '..' or end with '.'",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrows the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RepoName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RepoName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_names() {
        for ok in ["maven-central", "npm.proxy", "a", "Docker_hosted-1"] {
            assert!(RepoName::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_unsafe_names() {
        let long = "a".repeat(129);
        for bad in [
            "",
            "-x",
            ".x",
            "a/b",
            "a b",
            "a..b",
            "a.",
            "..",
            "é",
            long.as_str(),
        ] {
            assert!(RepoName::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn deserialize_validates() {
        assert!(serde_json::from_str::<RepoName>("\"ok-name\"").is_ok());
        assert!(serde_json::from_str::<RepoName>("\"../etc\"").is_err());
    }
}
