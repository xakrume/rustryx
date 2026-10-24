//! npm package names, with and without a scope.
//!
//! Two parsers, because the registry has two populations of names. [`PackageName::parse`] is the
//! lenient one used for reads and for proxied content: legacy names with capital letters and the
//! characters `~'!()*` exist on public registries and must stay reachable. [`PackageName::parse_new`]
//! is the strict one used for publishing: lowercase only, no special characters.
//!
//! Both refuse what would make a name ambiguous with the registry's own URL space: a leading `-`
//! (`/-/ping`, `/-/v1/search`), a leading `.` or `_`, a `.` or `..` segment, and anything that is not
//! URL safe. Specification: the name rules of the public `validate-npm-package-name` module
//! (<https://github.com/npm/validate-npm-package-name>) and the scoped package form of
//! <https://docs.npmjs.com/cli/v10/configuring-npm/package-json#name>.

use std::fmt;

use semver::Version;

/// Longest accepted name including the `@scope/` part (the limit npm documents).
pub const MAX_NAME_LEN: usize = 214;

/// Why a name was refused. For tests and logs; a client only ever sees a generic message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// Empty name or empty scope.
    #[error("empty name")]
    Empty,
    /// More than [`MAX_NAME_LEN`] bytes.
    #[error("name too long")]
    TooLong,
    /// A scope without a name, more than one slash, or a slash in an unscoped name.
    #[error("malformed scope")]
    Malformed,
    /// Leading `.`, `_` or `-`, or a segment made of dots.
    #[error("reserved leading character")]
    ReservedLead,
    /// A character outside the URL safe set.
    #[error("forbidden character")]
    ForbiddenChar,
    /// A capital letter in a name that is being published.
    #[error("uppercase letter in a new name")]
    Uppercase,
    /// One of the names the registry reserves (`node_modules`, `favicon.ico`).
    #[error("reserved name")]
    Reserved,
}

/// A validated package name: an optional scope (stored without the `@`) and a name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageName {
    scope: Option<String>,
    name: String,
}

impl PackageName {
    /// Parses a name the way a reader needs it: legacy spellings are accepted.
    ///
    /// # Errors
    /// The first rule that was violated.
    pub fn parse(full: &str) -> Result<Self, NameError> {
        Self::parse_inner(full, false)
    }

    /// Parses a name that is about to be created: lowercase, no `~'!()*`.
    ///
    /// # Errors
    /// The first rule that was violated.
    pub fn parse_new(full: &str) -> Result<Self, NameError> {
        Self::parse_inner(full, true)
    }

    fn parse_inner(full: &str, strict: bool) -> Result<Self, NameError> {
        if full.is_empty() {
            return Err(NameError::Empty);
        }
        if full.len() > MAX_NAME_LEN {
            return Err(NameError::TooLong);
        }
        if let Some(rest) = full.strip_prefix('@') {
            let (scope, name) = rest.split_once('/').ok_or(NameError::Malformed)?;
            validate_part(scope, strict)?;
            validate_part(name, strict)?;
            validate_not_reserved(name)?;
            Ok(Self {
                scope: Some(scope.to_owned()),
                name: name.to_owned(),
            })
        } else {
            if full.contains('/') {
                return Err(NameError::Malformed);
            }
            validate_part(full, strict)?;
            validate_not_reserved(full)?;
            Ok(Self {
                scope: None,
                name: full.to_owned(),
            })
        }
    }

    /// Builds a name from the one or two URL path segments that carry it (`["@scope","name"]` or
    /// `["name"]`). The caller has already decoded and normalized the path.
    ///
    /// # Errors
    /// As [`PackageName::parse`].
    pub fn from_segments(first: &str, second: Option<&str>) -> Result<Self, NameError> {
        match (first.starts_with('@'), second) {
            (true, Some(name)) => Self::parse(&format!("{first}/{name}")),
            (true, None) => Err(NameError::Malformed),
            (false, _) => Self::parse(first),
        }
    }

    /// Scope without the leading `@`.
    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// The part after the scope.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name as it appears in `package.json`: `@scope/name` or `name`.
    #[must_use]
    pub fn full(&self) -> String {
        self.to_string()
    }

    /// The form npm puts into a URL: the slash of a scoped name is `%2f`.
    #[must_use]
    pub fn escaped(&self) -> String {
        match &self.scope {
            Some(scope) => format!("@{scope}%2f{}", self.name),
            None => self.name.clone(),
        }
    }

    /// The form components are stored in and group claims are made in: lowercase scope and name.
    /// A claim made on the canonical form also covers a look-alike spelling with capitals.
    #[must_use]
    pub fn canonical(&self) -> Self {
        Self {
            scope: self.scope.as_ref().map(|s| s.to_ascii_lowercase()),
            name: self.name.to_ascii_lowercase(),
        }
    }

    /// File name of a version's tarball: `<name>-<version>.tgz`, without the scope.
    #[must_use]
    pub fn tarball_filename(&self, version: &Version) -> String {
        format!("{}-{version}.tgz", self.name)
    }

    /// Storage path and URL path of a version's tarball below the repository root.
    #[must_use]
    pub fn tarball_path(&self, version: &Version) -> String {
        format!("{self}/-/{}", self.tarball_filename(version))
    }

    /// Reads the version out of a tarball file name of this package.
    ///
    /// Accepts `<name>-<version>.tgz`. The version must be strict semver, so the name and the
    /// version cannot be confused however many dashes the name has.
    #[must_use]
    pub fn version_of_tarball(&self, file: &str) -> Option<Version> {
        let stem = file.strip_suffix(".tgz")?;
        let rest = stem.strip_prefix(self.name.as_str())?.strip_prefix('-')?;
        crate::version::parse_version(rest).ok()
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.scope {
            Some(scope) => write!(f, "@{scope}/{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

fn validate_part(part: &str, strict: bool) -> Result<(), NameError> {
    if part.is_empty() {
        return Err(NameError::Empty);
    }
    if part.bytes().all(|b| b == b'.') {
        return Err(NameError::ReservedLead);
    }
    if part.starts_with(['.', '_', '-']) {
        return Err(NameError::ReservedLead);
    }
    for byte in part.bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
        let legacy_special = matches!(byte, b'~' | b'\'' | b'!' | b'(' | b')' | b'*');
        if !(unreserved || (legacy_special && !strict)) {
            return Err(NameError::ForbiddenChar);
        }
        if strict && byte.is_ascii_uppercase() {
            return Err(NameError::Uppercase);
        }
    }
    Ok(())
}

fn validate_not_reserved(name: &str) -> Result<(), NameError> {
    if name.eq_ignore_ascii_case("node_modules") || name.eq_ignore_ascii_case("favicon.ico") {
        return Err(NameError::Reserved);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_scoped_names() {
        let plain = PackageName::parse("left-pad").unwrap();
        assert_eq!(plain.scope(), None);
        assert_eq!(plain.name(), "left-pad");
        assert_eq!(plain.full(), "left-pad");
        let scoped = PackageName::parse("@acme/billing.core").unwrap();
        assert_eq!(scoped.scope(), Some("acme"));
        assert_eq!(scoped.name(), "billing.core");
        assert_eq!(scoped.full(), "@acme/billing.core");
        assert_eq!(scoped.escaped(), "@acme%2fbilling.core");
        assert_eq!(plain.escaped(), "left-pad");
    }

    #[test]
    fn legacy_names_are_readable_but_not_publishable() {
        for legacy in ["JSONStream", "Base64", "what's-up", "a~b", "x!(y)*"] {
            assert!(PackageName::parse(legacy).is_ok(), "{legacy}");
            assert!(PackageName::parse_new(legacy).is_err(), "{legacy}");
        }
        assert_eq!(
            PackageName::parse_new("JSONStream"),
            Err(NameError::Uppercase)
        );
        assert_eq!(PackageName::parse_new("a~b"), Err(NameError::ForbiddenChar));
    }

    #[test]
    fn refuses_names_that_collide_with_the_registry_url_space() {
        for bad in [
            "",
            "-",
            "-/ping",
            ".hidden",
            "_private",
            "..",
            ".",
            "a/b",
            "@scope",
            "@/x",
            "@scope/",
            "@scope/-x",
            "@a/b/c",
            "@@a/b",
            "a b",
            "a%2fb",
            "a\\b",
            "a\u{0}b",
            "node_modules",
            "favicon.ico",
            "@s/node_modules",
            "ünï",
        ] {
            assert!(PackageName::parse(bad).is_err(), "{bad:?}");
            assert!(PackageName::parse_new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn length_is_bounded_including_the_scope() {
        let ok = "a".repeat(MAX_NAME_LEN);
        assert!(PackageName::parse(&ok).is_ok());
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert_eq!(PackageName::parse(&long), Err(NameError::TooLong));
        let scoped = format!("@{}/{}", "s".repeat(100), "n".repeat(120));
        assert_eq!(PackageName::parse(&scoped), Err(NameError::TooLong));
    }

    #[test]
    fn from_segments_joins_scope_and_name() {
        assert_eq!(
            PackageName::from_segments("@a", Some("b")).unwrap().full(),
            "@a/b"
        );
        assert_eq!(PackageName::from_segments("a", None).unwrap().full(), "a");
        assert!(PackageName::from_segments("@a", None).is_err());
        assert!(PackageName::from_segments("a", Some("b")).is_ok());
    }

    #[test]
    fn canonical_lowercases_both_parts_and_is_idempotent() {
        let name = PackageName::parse("@Acme/Billing").unwrap();
        let canonical = name.canonical();
        assert_eq!(canonical.full(), "@acme/billing");
        assert_eq!(canonical.canonical(), canonical);
    }

    #[test]
    fn tarball_names_never_carry_the_scope_in_the_file() {
        let name = PackageName::parse("@acme/billing").unwrap();
        let version = Version::parse("1.2.3-beta.1").unwrap();
        assert_eq!(name.tarball_filename(&version), "billing-1.2.3-beta.1.tgz");
        assert_eq!(
            name.tarball_path(&version),
            "@acme/billing/-/billing-1.2.3-beta.1.tgz"
        );
        assert_eq!(
            name.version_of_tarball("billing-1.2.3-beta.1.tgz"),
            Some(version)
        );
    }

    #[test]
    fn tarball_version_is_read_from_the_end_even_when_the_name_has_dashes() {
        let name = PackageName::parse("a-1-b").unwrap();
        assert_eq!(
            name.version_of_tarball("a-1-b-2.0.0.tgz"),
            Some(Version::new(2, 0, 0))
        );
        assert_eq!(name.version_of_tarball("a-1-b-x.tgz"), None);
        assert_eq!(name.version_of_tarball("a-1-b-2.0.0.tar"), None);
        assert_eq!(name.version_of_tarball("other-2.0.0.tgz"), None);
        assert_eq!(name.version_of_tarball("a-1-b2.0.0.tgz"), None);
    }
}
