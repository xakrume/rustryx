//! Tags and references.
//!
//! A tag is `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}` (OCI Distribution Specification 1.1). A
//! reference is a tag or a digest; any `:` makes it a digest attempt, because a tag cannot
//! contain one.

use std::fmt;

use super::digest::{Digest, DigestError};

/// Longest tag.
pub const MAX_TAG_LEN: usize = 128;

/// Why a reference was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceError {
    /// Not a valid tag.
    #[error("invalid tag")]
    InvalidTag,
    /// Contains `:` but is not a valid digest.
    #[error("invalid digest reference: {0}")]
    InvalidDigest(DigestError),
}

/// A validated tag.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tag(String);

impl Tag {
    /// Parses a tag.
    ///
    /// # Errors
    /// [`ReferenceError::InvalidTag`] when the text is empty, too long or has a character
    /// outside the grammar.
    pub fn parse(text: &str) -> Result<Self, ReferenceError> {
        if !is_tag(text) {
            return Err(ReferenceError::InvalidTag);
        }
        Ok(Self(text.to_owned()))
    }

    /// The tag as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// True when `text` is a valid tag.
#[must_use]
pub fn is_tag(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    if bytes.len() > MAX_TAG_LEN || !(first.is_ascii_alphanumeric() || *first == b'_') {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// What a manifest request names: a mutable tag or an immutable digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Reference {
    /// A tag.
    Tag(Tag),
    /// A content digest.
    Digest(Digest),
}

impl Reference {
    /// Parses a path segment as a reference.
    ///
    /// # Errors
    /// [`ReferenceError::InvalidDigest`] when the text has a `:` and is no digest,
    /// [`ReferenceError::InvalidTag`] otherwise.
    pub fn parse(text: &str) -> Result<Self, ReferenceError> {
        if text.contains(':') {
            Digest::parse(text)
                .map(Self::Digest)
                .map_err(ReferenceError::InvalidDigest)
        } else {
            Tag::parse(text).map(Self::Tag)
        }
    }

    /// The digest, when this is one.
    #[must_use]
    pub fn digest(&self) -> Option<&Digest> {
        match self {
            Self::Digest(digest) => Some(digest),
            Self::Tag(_) => None,
        }
    }

    /// The reference as it appears in a path.
    #[must_use]
    pub fn as_path_segment(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag(tag) => tag.fmt(f),
            Self::Digest(digest) => digest.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests;
