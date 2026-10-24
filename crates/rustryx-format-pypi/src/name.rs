//! Project names: validation and the PEP 503 normalization.
//!
//! Specifications:
//! * Name normalization: <https://packaging.python.org/en/latest/specifications/name-normalization/>
//!   (`re.sub(r"[-_.]+", "-", name).lower()`).
//! * Valid names: <https://packaging.python.org/en/latest/specifications/name-normalization/#valid-non-normalized-names>
//!   (ASCII letters, digits, `-`, `_`, `.`; starting and ending with a letter or digit).
//! * Simple repository API: <https://packaging.python.org/en/latest/specifications/simple-repository-api/>
//!
//! Invariant: a [`ProjectName`] always holds the normalized form, so that the name a request
//! is authorized for, the name a component is stored under and the name a dependency
//! confusion claim is made for are one string. A look-alike spelling (`Foo_Bar`, `foo.bar`)
//! can never address a different project than `foo-bar`.

use std::fmt;

/// Longest project name accepted. Real names are far shorter; the cap bounds paths, component
/// rows and index lines. It is below the 255 byte limit of one path segment, which also holds
/// the file names derived from the name.
pub const MAX_NAME_LEN: usize = 128;

/// Why a project name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// The name is empty.
    #[error("empty project name")]
    Empty,
    /// The name is longer than [`MAX_NAME_LEN`].
    #[error("project name too long")]
    TooLong,
    /// The name contains a character outside ASCII letters, digits, `-`, `_` and `.`.
    #[error("project name contains a forbidden character")]
    ForbiddenChar,
    /// The name does not start or does not end with a letter or digit.
    #[error("project name must start and end with a letter or digit")]
    BadEdge,
}

/// A valid project name in normalized form (lowercase, runs of `-`, `_`, `.` collapsed to one `-`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectName(String);

impl ProjectName {
    /// Validates and normalizes `raw`.
    ///
    /// # Errors
    /// [`NameError`] for an empty, too long or malformed name.
    pub fn parse(raw: &str) -> Result<Self, NameError> {
        validate(raw)?;
        Ok(Self(normalize(raw)))
    }

    /// The normalized name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name as a wheel and `.dist-info` directory spell it: normalized, with `-` replaced by
    /// `_` (binary distribution format, "Escaping and Unicode").
    #[must_use]
    pub fn wheel_escaped(&self) -> String {
        self.0.replace('-', "_")
    }
}

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// PEP 503 normalization of any text: ASCII lowercase, every run of `-`, `_` and `.` becomes one
/// `-`. It does not validate, and it is idempotent: `normalize(normalize(x)) == normalize(x)`.
///
/// Only ASCII is lowercased on purpose. Unicode lowercasing is not idempotent for every code
/// point, and a name with a non-ASCII character is refused by [`ProjectName::parse`] anyway.
#[must_use]
pub fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_separator = false;
    for ch in raw.chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !in_separator {
                out.push('-');
            }
            in_separator = true;
        } else {
            out.push(ch.to_ascii_lowercase());
            in_separator = false;
        }
    }
    out
}

fn validate(raw: &str) -> Result<(), NameError> {
    if raw.is_empty() {
        return Err(NameError::Empty);
    }
    if raw.len() > MAX_NAME_LEN {
        return Err(NameError::TooLong);
    }
    if !raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(NameError::ForbiddenChar);
    }
    let edge_ok = |b: Option<&u8>| b.is_some_and(u8::is_ascii_alphanumeric);
    if !(edge_ok(raw.as_bytes().first()) && edge_ok(raw.as_bytes().last())) {
        return Err(NameError::BadEdge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn normalizes_like_pep_503() {
        for (raw, expected) in [
            ("Foo_Bar", "foo-bar"),
            ("foo.bar", "foo-bar"),
            ("FOO--BAR", "foo-bar"),
            ("a-_.-b", "a-b"),
            ("Django", "django"),
            ("zope.interface", "zope-interface"),
            ("a", "a"),
        ] {
            assert_eq!(ProjectName::parse(raw).unwrap().as_str(), expected, "{raw}");
        }
    }

    #[test]
    fn refuses_malformed_names() {
        assert_eq!(ProjectName::parse(""), Err(NameError::Empty));
        assert_eq!(ProjectName::parse("-a"), Err(NameError::BadEdge));
        assert_eq!(ProjectName::parse("a-"), Err(NameError::BadEdge));
        assert_eq!(ProjectName::parse("a b"), Err(NameError::ForbiddenChar));
        assert_eq!(ProjectName::parse("a/b"), Err(NameError::ForbiddenChar));
        assert_eq!(ProjectName::parse("a%2fb"), Err(NameError::ForbiddenChar));
        assert_eq!(ProjectName::parse("é"), Err(NameError::ForbiddenChar));
        assert_eq!(
            ProjectName::parse(&"a".repeat(MAX_NAME_LEN + 1)),
            Err(NameError::TooLong)
        );
        assert!(ProjectName::parse(&"a".repeat(MAX_NAME_LEN)).is_ok());
    }

    #[test]
    fn wheel_escaping_uses_underscores() {
        let name = ProjectName::parse("My.Pkg-name").unwrap();
        assert_eq!(name.wheel_escaped(), "my_pkg_name");
        assert_eq!(name.to_string(), "my-pkg-name");
    }

    proptest! {
        #[test]
        fn normalization_is_idempotent(raw in ".{0,64}") {
            let once = normalize(&raw);
            prop_assert_eq!(normalize(&once), once);
        }

        #[test]
        fn spellings_of_one_name_agree(parts in proptest::collection::vec("[A-Za-z0-9]{1,8}", 1..5),
                                       seps in proptest::collection::vec("[-_.]{1,3}", 4)) {
            let mut a = String::new();
            let mut b = String::new();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    a.push_str(seps.get(i - 1).map_or("-", String::as_str));
                    b.push('-');
                }
                a.push_str(part);
                b.push_str(&part.to_ascii_lowercase());
            }
            let left = ProjectName::parse(&a).unwrap();
            let right = ProjectName::parse(&b).unwrap();
            prop_assert_eq!(left, right);
        }

        #[test]
        fn parse_never_panics(raw in "\\PC{0,200}") {
            let _ = ProjectName::parse(&raw);
        }

        #[test]
        fn parsed_names_are_fixed_points(raw in "[A-Za-z0-9][A-Za-z0-9._-]{0,30}[A-Za-z0-9]") {
            let name = ProjectName::parse(&raw).unwrap();
            prop_assert_eq!(ProjectName::parse(name.as_str()).unwrap(), name);
        }
    }
}
