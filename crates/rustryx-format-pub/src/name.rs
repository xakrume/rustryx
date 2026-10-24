//! Package names.
//!
//! The rule is the one of the Hosted Pub Repository Specification Version 2 as the format
//! requirements of this project state it: lowercase ASCII letters, digits and underscore, the
//! first character a letter. Nothing is repaired or case-folded, because a name that two layers
//! normalise differently is a name that escapes a group name claim (dependency confusion): the
//! only spelling that exists is the canonical one, so [`PackageName::parse`] either returns
//! the input unchanged or refuses it.

use std::fmt;
use std::str::FromStr;

/// Longest accepted package name, in bytes. The specification sets no limit; 64 is the limit of
/// the public registry, and it keeps a name far below the path segment limit of format-core.
pub const MAX_PACKAGE_NAME_LEN: usize = 64;

/// Why a package name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// The name is empty.
    #[error("package name is empty")]
    Empty,
    /// The name is longer than [`MAX_PACKAGE_NAME_LEN`].
    #[error("package name is longer than {MAX_PACKAGE_NAME_LEN} characters")]
    TooLong,
    /// The first character is not a lowercase letter.
    #[error("package name must start with a lowercase letter")]
    BadStart,
    /// A character other than a lowercase letter, a digit or an underscore.
    #[error("package name may contain only lowercase letters, digits and underscores")]
    BadChar,
}

/// A valid package name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageName(String);

impl PackageName {
    /// Parses a package name.
    ///
    /// # Errors
    /// The [`NameError`] of the first rule the text breaks.
    pub fn parse(raw: &str) -> Result<Self, NameError> {
        if raw.is_empty() {
            return Err(NameError::Empty);
        }
        if raw.len() > MAX_PACKAGE_NAME_LEN {
            return Err(NameError::TooLong);
        }
        let mut bytes = raw.bytes();
        if !bytes.next().is_some_and(|b| b.is_ascii_lowercase()) {
            return Err(NameError::BadStart);
        }
        if !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return Err(NameError::BadChar);
        }
        Ok(Self(raw.to_owned()))
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PackageName {
    type Err = NameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl AsRef<str> for PackageName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_documented_alphabet() {
        for name in ["a", "http", "flutter_bloc", "a1", "a_1_b", "x9"] {
            assert_eq!(PackageName::parse(name).unwrap().as_str(), name);
        }
        assert_eq!(PackageName::parse("pkg").unwrap().to_string(), "pkg");
        assert_eq!("pkg".parse::<PackageName>().unwrap().as_ref(), "pkg");
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(PackageName::parse(""), Err(NameError::Empty));
        for name in ["1a", "_a", "-a", "A", "Éa"] {
            assert_eq!(PackageName::parse(name), Err(NameError::BadStart), "{name}");
        }
        for name in [
            "aB", "a-b", "a.b", "a b", "a/b", "a\\b", "a%41", "a\0", "aé", "a\n",
        ] {
            assert_eq!(
                PackageName::parse(name),
                Err(NameError::BadChar),
                "{name:?}"
            );
        }
        assert_eq!(
            PackageName::parse(&"a".repeat(MAX_PACKAGE_NAME_LEN + 1)),
            Err(NameError::TooLong)
        );
        assert!(PackageName::parse(&"a".repeat(MAX_PACKAGE_NAME_LEN)).is_ok());
    }

    #[test]
    fn error_texts_are_client_safe() {
        for error in [
            NameError::Empty,
            NameError::TooLong,
            NameError::BadStart,
            NameError::BadChar,
        ] {
            assert!(error.to_string().starts_with("package name"));
        }
    }
}
