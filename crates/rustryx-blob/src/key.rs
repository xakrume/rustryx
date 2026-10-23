//! Blob key validation and the key-to-location mapping shared by both stores.
//!
//! Keys are opaque (`UUIDv7` text, or a few fixed-prefix staging names) but they
//! arrive from other crates, so every store treats them as untrusted input before
//! turning them into a path or an object name.
//!
//! Two decisions live here:
//!
//! 1. **Strict allowlist.** A component may contain only `[A-Za-z0-9._-]`. That
//!    excludes NUL, control characters, backslash (a separator on Windows) and every
//!    non-ASCII character. The last point matters on macOS and Windows, where
//!    Unicode normalisation and case folding can make two different keys land on one
//!    file.
//! 2. **Hash sharding.** The location is `<b0>/<b1>/<key>` where `b0` and `b1` are the
//!    first two bytes of SHA-256 of the key, in hex. A literal key prefix would not
//!    work: `UUIDv7` starts with a millisecond timestamp, so every key written in
//!    the same ~49 days shares its first eight hex digits and would pile into one
//!    directory (and, on S3, into one hot prefix).

use rustryx_core::ports::BlobKey;
use sha2::{Digest, Sha256};

/// Longest accepted key in bytes.
pub const MAX_KEY_BYTES: usize = 512;

/// Longest accepted path component in bytes. Below the 255 byte file name limit so
/// that no file system rejects a valid key.
pub const MAX_COMPONENT_BYTES: usize = 200;

/// Name of the staging directory inside a local store root. Reserved as the first
/// component of a key so that a staging identifier can never name a stored blob.
pub const STAGING_DIR: &str = "_staging";

/// Why a key was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The key is the empty string.
    #[error("blob key is empty")]
    Empty,
    /// The key is longer than [`MAX_KEY_BYTES`].
    #[error("blob key is longer than the allowed maximum")]
    TooLong,
    /// The key starts with a path separator.
    #[error("blob key must not be an absolute path")]
    Absolute,
    /// The key has an empty component (`a//b` or a trailing separator).
    #[error("blob key has an empty path component")]
    EmptyComponent,
    /// The key has a `.` or `..` component.
    #[error("blob key has a relative path component")]
    DotComponent,
    /// A component is longer than [`MAX_COMPONENT_BYTES`].
    #[error("blob key has a path component that is too long")]
    ComponentTooLong,
    /// A component contains a character outside `[A-Za-z0-9._-]` (this includes NUL).
    #[error("blob key contains a character outside the allowed set")]
    ForbiddenCharacter,
    /// The first component is the reserved staging directory name.
    #[error("blob key uses the reserved staging component")]
    Reserved,
}

fn is_allowed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn validate_component(component: &str) -> Result<(), KeyError> {
    if component.is_empty() {
        return Err(KeyError::EmptyComponent);
    }
    if component == "." || component == ".." {
        return Err(KeyError::DotComponent);
    }
    if component.len() > MAX_COMPONENT_BYTES {
        return Err(KeyError::ComponentTooLong);
    }
    if !component.bytes().all(is_allowed) {
        return Err(KeyError::ForbiddenCharacter);
    }
    Ok(())
}

/// Validates a key and returns its text.
///
/// Rejects the empty key, absolute keys, `.` and `..` components, empty components,
/// NUL and every character outside `[A-Za-z0-9._-]`, over-long keys, and keys whose
/// first component is [`STAGING_DIR`].
pub fn validate_key(key: &BlobKey) -> Result<&str, KeyError> {
    let text = key.as_str();
    if text.is_empty() {
        return Err(KeyError::Empty);
    }
    if text.len() > MAX_KEY_BYTES {
        return Err(KeyError::TooLong);
    }
    if text.starts_with('/') {
        return Err(KeyError::Absolute);
    }
    for component in text.split('/') {
        validate_component(component)?;
    }
    if text.split('/').next() == Some(STAGING_DIR) {
        return Err(KeyError::Reserved);
    }
    Ok(text)
}

/// Validates an object name prefix for S3 (`a/b`, no leading or trailing slash).
/// The empty prefix is valid and means "bucket root".
pub(crate) fn validate_prefix(prefix: &str) -> Result<(), KeyError> {
    if prefix.is_empty() {
        return Ok(());
    }
    if prefix.starts_with('/') {
        return Err(KeyError::Absolute);
    }
    prefix.split('/').try_for_each(validate_component)
}

/// The two shard directory names of a key: first two bytes of SHA-256, hex.
#[must_use]
pub fn shard_of(key: &str) -> (String, String) {
    let digest = Sha256::digest(key.as_bytes());
    (hex::encode([digest[0]]), hex::encode([digest[1]]))
}

/// Location of a key relative to the store root: `<b0>/<b1>/<key>`. Used verbatim
/// as the object name suffix on S3. The key must already be valid.
#[must_use]
pub(crate) fn relative_location(key: &str) -> String {
    let (first, second) = shard_of(key);
    format!("{first}/{second}/{key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) -> Result<(), KeyError> {
        validate_key(&BlobKey::new(text)).map(|_| ())
    }

    #[test]
    fn accepts_uuid_and_nested_keys() {
        assert_eq!(check("0192f3a4-7b1c-7d2e-8f00-123456789abc"), Ok(()));
        assert_eq!(check("_uploads/docker/0192f3a4-7b1c"), Ok(()));
        assert_eq!(check("a.b_c-d"), Ok(()));
    }

    #[test]
    fn must_not_accept_keys_that_could_escape_the_root() {
        assert_eq!(check(""), Err(KeyError::Empty));
        assert_eq!(check("/etc/passwd"), Err(KeyError::Absolute));
        assert_eq!(check(".."), Err(KeyError::DotComponent));
        assert_eq!(check("../x"), Err(KeyError::DotComponent));
        assert_eq!(check("a/../x"), Err(KeyError::DotComponent));
        assert_eq!(check("a/./x"), Err(KeyError::DotComponent));
        assert_eq!(check("a//x"), Err(KeyError::EmptyComponent));
        assert_eq!(check("a/"), Err(KeyError::EmptyComponent));
        assert_eq!(check("a\0b"), Err(KeyError::ForbiddenCharacter));
        assert_eq!(check("a\\b"), Err(KeyError::ForbiddenCharacter));
        assert_eq!(check("a b"), Err(KeyError::ForbiddenCharacter));
        assert_eq!(check("caf\u{e9}"), Err(KeyError::ForbiddenCharacter));
        assert_eq!(check("a\nb"), Err(KeyError::ForbiddenCharacter));
    }

    #[test]
    fn must_not_accept_oversized_keys() {
        assert_eq!(
            check(&"a".repeat(MAX_KEY_BYTES + 1)),
            Err(KeyError::TooLong)
        );
        assert_eq!(
            check(&"a".repeat(MAX_COMPONENT_BYTES + 1)),
            Err(KeyError::ComponentTooLong)
        );
        assert_eq!(check(&"a".repeat(MAX_COMPONENT_BYTES)), Ok(()));
    }

    #[test]
    fn must_not_accept_the_reserved_staging_component() {
        assert_eq!(check("_staging"), Err(KeyError::Reserved));
        assert_eq!(check("_staging/abc.part"), Err(KeyError::Reserved));
        assert_eq!(check("x/_staging"), Ok(()));
    }

    #[test]
    fn prefix_validation_follows_key_rules() {
        assert_eq!(validate_prefix(""), Ok(()));
        assert_eq!(validate_prefix("tenant-1/blobs"), Ok(()));
        assert_eq!(validate_prefix("/abs"), Err(KeyError::Absolute));
        assert_eq!(validate_prefix("a/"), Err(KeyError::EmptyComponent));
        assert_eq!(validate_prefix("a/../b"), Err(KeyError::DotComponent));
    }

    #[test]
    fn shard_is_stable_and_spreads_uuid_v7_prefixes() {
        assert_eq!(shard_of("abc"), ("ba".to_string(), "78".to_string()));
        let shards: std::collections::HashSet<_> = (0..512)
            .map(|i| shard_of(&format!("0192f3a4-7b1c-7d2e-8f00-{i:012x}")))
            .collect();
        // Keys sharing the first 24 characters must still fan out.
        assert!(shards.len() > 200, "only {} distinct shards", shards.len());
    }

    #[test]
    fn relative_location_has_two_shard_levels() {
        let location = relative_location("abc");
        assert_eq!(location, "ba/78/abc");
    }
}
