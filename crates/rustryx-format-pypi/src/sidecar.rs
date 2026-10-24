//! The two small records that live next to a distribution as assets.
//!
//! The asset row of a file has no place for what an index page needs beyond size and digests, so
//! the repository keeps it in assets of its own, in the same project directory:
//!
//! * `<file>.info.json`: written once at upload, holds the `requires-python` the client sent;
//! * `<file>.yanked`: exists exactly while the file is yanked, holds the reason (PEP 592).
//!
//! A yank marker is created and deleted, never rewritten: that works under the `allow_once`
//! write policy, where an overwrite is refused, and it needs no read-modify-write of a shared
//! document, so two maintainers cannot lose each other's change.

use serde::{Deserialize, Serialize};

/// Longest yank reason kept.
pub const MAX_YANK_REASON_BYTES: usize = 1024;
/// Largest sidecar read back; both records are far smaller, the bound protects the index
/// generator from an asset somebody else wrote at the path.
pub const MAX_SIDECAR_BYTES: u64 = 64 * 1024;

/// What the index needs about a distribution and the asset row does not say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    /// Version of this record's layout.
    #[serde(default)]
    pub v: u8,
    /// `Requires-Python` as the client sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_python: Option<String>,
    /// `Metadata-Version` of the distribution, as sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_version: Option<String>,
}

impl FileInfo {
    /// The record for a new upload.
    #[must_use]
    pub fn new(requires_python: Option<String>, metadata_version: Option<String>) -> Self {
        Self {
            v: 1,
            requires_python,
            metadata_version,
        }
    }

    /// The bytes that are stored.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Reads a stored record. A record that cannot be read is an empty one: the file is still
    /// served and listed, it just carries no `requires-python`.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        serde_json::from_slice(bytes).unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize)]
struct YankRecord {
    #[serde(default)]
    reason: String,
}

/// Why a yank reason was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReasonError {
    /// Longer than [`MAX_YANK_REASON_BYTES`].
    #[error("the yank reason is too long")]
    TooLong,
    /// Contains a control character other than a space.
    #[error("the yank reason contains a control character")]
    ControlCharacter,
}

/// Validates a yank reason: it ends up in an HTML attribute and a JSON string, so control
/// characters are refused; length is bounded.
///
/// # Errors
/// [`ReasonError`].
pub fn validate_reason(reason: &str) -> Result<&str, ReasonError> {
    let reason = reason.trim();
    if reason.len() > MAX_YANK_REASON_BYTES {
        return Err(ReasonError::TooLong);
    }
    if reason.chars().any(char::is_control) {
        return Err(ReasonError::ControlCharacter);
    }
    Ok(reason)
}

/// The bytes of a yank marker for `reason`.
#[must_use]
pub fn yank_marker(reason: &str) -> Vec<u8> {
    serde_json::to_vec(&YankRecord {
        reason: reason.to_owned(),
    })
    .unwrap_or_default()
}

/// The reason in a stored yank marker. An unreadable marker still means "yanked", with no reason.
#[must_use]
pub fn yank_reason(bytes: &[u8]) -> String {
    serde_json::from_slice::<YankRecord>(bytes)
        .map(|record| record.reason)
        .ok()
        .and_then(|reason| validate_reason(&reason).ok().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_info_round_trips_and_tolerates_garbage() {
        let info = FileInfo::new(Some(">=3.8".into()), Some("2.1".into()));
        assert_eq!(FileInfo::from_bytes(&info.to_bytes()), info);
        assert_eq!(FileInfo::from_bytes(b"not json"), FileInfo::default());
        assert_eq!(
            FileInfo::from_bytes(b"{\"requires_python\":5}"),
            FileInfo::default()
        );
        assert_eq!(
            FileInfo::from_bytes(b"{\"future\":1,\"requires_python\":\"x\"}")
                .requires_python
                .as_deref(),
            Some("x")
        );
        assert_eq!(FileInfo::new(None, None).to_bytes(), b"{\"v\":1}");
    }

    #[test]
    fn reasons_are_validated_and_trimmed() {
        assert_eq!(validate_reason("  broken build ").unwrap(), "broken build");
        assert_eq!(validate_reason("").unwrap(), "");
        assert_eq!(validate_reason("a\nb"), Err(ReasonError::ControlCharacter));
        assert_eq!(
            validate_reason("a\u{7}"),
            Err(ReasonError::ControlCharacter)
        );
        assert_eq!(
            validate_reason(&"x".repeat(MAX_YANK_REASON_BYTES + 1)),
            Err(ReasonError::TooLong)
        );
        assert!(validate_reason(&"x".repeat(MAX_YANK_REASON_BYTES)).is_ok());
    }

    #[test]
    fn markers_carry_the_reason_and_a_bad_marker_still_yanks() {
        assert_eq!(yank_reason(&yank_marker("security fix")), "security fix");
        assert_eq!(yank_reason(&yank_marker("")), "");
        assert_eq!(yank_reason(b"garbage"), "");
        assert_eq!(
            yank_reason(b"{\"reason\":\"a\\nb\"}"),
            "",
            "a reason with a control character is dropped"
        );
    }
}
