//! Upload session ids.
//!
//! The id in `.../blobs/uploads/<id>` is the token of a `ChunkedSession` of format-core
//! (`repository-uuid.blob-uuid.store-handle`). This parser only says whether a path
//! segment can be such an id at all (length and alphabet); whether it names a live
//! session of the repository is decided by `ChunkedSession::parse` and the store.

use std::fmt;

/// Longest id: two UUIDs, two dots and a store handle of at most 128 bytes, plus room.
pub const MAX_SESSION_ID_LEN: usize = 256;

/// Why an upload session id was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid upload session id")]
pub struct SessionIdError;

/// A path segment that can name an upload session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UploadSessionId(String);

impl UploadSessionId {
    /// Validates a path segment: 1 to 256 characters of `A-Z a-z 0-9 . _ ~ -`.
    ///
    /// # Errors
    /// [`SessionIdError`] for anything else.
    pub fn parse(text: &str) -> Result<Self, SessionIdError> {
        let ok = !text.is_empty()
            && text.len() <= MAX_SESSION_ID_LEN
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'));
        if ok {
            Ok(Self(text.to_owned()))
        } else {
            Err(SessionIdError)
        }
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UploadSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests;
