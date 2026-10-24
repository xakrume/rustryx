//! Repository (image) names.
//!
//! Grammar, from the OCI Distribution Specification 1.1
//! (<https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md>, "Pulling
//! manifests" and the conformance regular expressions):
//!
//! ```text
//! name      := component ("/" component)*
//! component := [a-z0-9]+ ( separator [a-z0-9]+ )*
//! separator := "." | "_" | "__" | "-"+
//! ```
//!
//! Uppercase letters are not allowed: two spellings of one name would be two images.
//! The total length is capped at 255 bytes, the limit Docker documents for a repository
//! name. The parser walks the bytes once and never indexes past the end.
//!
//! In Rustryx the name a client sends has the Rustryx repository as its first
//! component (`<rustryx-repo>/<image path>`); [`split_full_name`] separates the two. A
//! [`RepositoryName`] is the image path *inside* a repository.

use std::fmt;

use rustryx_core::names::RepoName;

/// Longest name, in bytes.
pub const MAX_NAME_LEN: usize = 255;

/// Why a name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// Empty, too long, or a component does not fit the grammar.
    #[error("invalid repository name")]
    Invalid,
    /// A full name without both a repository and an image path.
    #[error("the name needs a repository and an image path")]
    MissingImage,
}

/// A validated image path such as `team/app`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepositoryName(String);

impl RepositoryName {
    /// Parses an image path.
    ///
    /// # Errors
    /// [`NameError::Invalid`] when the text is empty, longer than [`MAX_NAME_LEN`] or any
    /// component breaks the grammar.
    pub fn parse(text: &str) -> Result<Self, NameError> {
        if text.is_empty() || text.len() > MAX_NAME_LEN {
            return Err(NameError::Invalid);
        }
        if text.split('/').all(valid_component) {
            Ok(Self(text.to_owned()))
        } else {
            Err(NameError::Invalid)
        }
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last path component (`app` for `team/app`).
    #[must_use]
    pub fn leaf(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
}

impl fmt::Display for RepositoryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `[a-z0-9]+ ( ("." | "_" | "__" | "-"+) [a-z0-9]+ )*`
fn valid_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let mut i = 0;
    // The component must start with an alphanumeric run.
    let Some(first) = bytes.first() else {
        return false;
    };
    if !alnum(*first) {
        return false;
    }
    while i < bytes.len() && alnum(bytes[i]) {
        i += 1;
    }
    while i < bytes.len() {
        // One separator: `.`, `_`, `__` or a run of `-`.
        match bytes[i] {
            b'.' => i += 1,
            b'_' => {
                i += 1;
                if bytes.get(i) == Some(&b'_') {
                    i += 1;
                }
            }
            b'-' => {
                while bytes.get(i) == Some(&b'-') {
                    i += 1;
                }
            }
            _ => return false,
        }
        // A separator must be followed by an alphanumeric run.
        let start = i;
        while i < bytes.len() && alnum(bytes[i]) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    true
}

/// Splits `<rustryx-repo>/<image path>` into its two parts.
///
/// The repository part must be a valid Rustryx [`RepoName`] that is also lowercase: a
/// registry client cannot send a name with an uppercase letter, so a repository whose
/// name has one is unreachable through the docker protocol and is never matched here.
///
/// # Errors
/// [`NameError::MissingImage`] when there is no `/`, [`NameError::Invalid`] when either
/// part is invalid.
pub fn split_full_name(full: &str) -> Result<(RepoName, RepositoryName), NameError> {
    if full.len() > MAX_NAME_LEN + 1 + rustryx_core::names::MAX_REPO_NAME_LEN {
        return Err(NameError::Invalid);
    }
    let (repository, image) = full.split_once('/').ok_or(NameError::MissingImage)?;
    if repository.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(NameError::Invalid);
    }
    let repository = RepoName::parse(repository).map_err(|_| NameError::Invalid)?;
    Ok((repository, RepositoryName::parse(image)?))
}

#[cfg(test)]
mod tests;
