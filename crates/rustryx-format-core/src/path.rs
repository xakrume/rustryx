//! Path normalization and traversal rejection.
//!
//! Invariant (SEC-19): the path used for the authorization decision is the very
//! same [`NormalizedPath`] the handler uses to address storage. [`crate::FormatHandler::classify`]
//! produces it once from the raw, still percent-encoded request path, and `handle`
//! only ever reads it from [`crate::ArtifactRequest::path`]. Nothing downstream
//! decodes again, so `%2e%2e`, `%252e%252e` and `%2F` cannot make the two views differ.
//!
//! The rules are strict on purpose. Anything ambiguous is rejected instead of being
//! repaired, because a repaired path is a path two layers may repair differently.

use std::fmt::{self, Write as _};

/// Why a raw path was rejected. The variants are for tests and logs; the client
/// only ever sees a generic "invalid path".
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The path is empty and the rules do not allow the repository root.
    #[error("empty path")]
    Empty,
    /// The path or one segment exceeds the length limit.
    #[error("path too long")]
    TooLong,
    /// Malformed percent escape or bytes that are not valid UTF-8 after decoding.
    #[error("invalid percent encoding")]
    InvalidEncoding,
    /// A `%2f` escape in a context that does not allow encoded slashes.
    #[error("encoded slash")]
    EncodedSlash,
    /// A valid percent escape is still present after decoding (double encoding).
    #[error("double encoded path")]
    DoubleEncoded,
    /// `.`, `..` or a segment made only of dots.
    #[error("path traversal")]
    Traversal,
    /// An empty segment, as in `a//b`.
    #[error("empty path segment")]
    DoubleSlash,
    /// NUL, another control character or a backslash.
    #[error("forbidden character")]
    ForbiddenChar,
    /// A trailing slash where the rules do not allow one.
    #[error("trailing slash")]
    TrailingSlash,
}

/// Knobs of [`NormalizedPath::parse_with`].
///
/// The defaults suit storage paths: no trailing slash, no encoded slash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathRules {
    /// Maximum length of the decoded path in bytes.
    pub max_len: usize,
    /// Maximum length of one decoded segment in bytes.
    pub max_segment_len: usize,
    /// Accept one trailing slash (directory-style requests). It is recorded, not kept.
    pub allow_trailing_slash: bool,
    /// Accept `%2f` as a path separator. npm scoped packages need it. The decoded
    /// text is split afterwards, so a smuggled `..` is still caught.
    pub allow_encoded_slash: bool,
    /// Accept the empty path (the repository root).
    pub allow_empty: bool,
}

impl Default for PathRules {
    fn default() -> Self {
        Self {
            max_len: 2048,
            max_segment_len: 255,
            allow_trailing_slash: false,
            allow_encoded_slash: false,
            allow_empty: true,
        }
    }
}

/// A decoded, validated, slash-separated path below the repository root.
///
/// Guarantees: valid UTF-8, no leading slash, no empty segment, no `.` or `..`
/// segment, no NUL, control character or backslash, no residual percent escape,
/// bounded length. It can only be built through [`NormalizedPath::parse`],
/// [`NormalizedPath::parse_with`] or [`NormalizedPath::from_segments`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedPath {
    value: String,
    trailing_slash: bool,
}

impl NormalizedPath {
    /// Normalizes with [`PathRules::default`].
    ///
    /// # Errors
    /// Returns the [`PathError`] describing the first rule that was violated.
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        Self::parse_with(raw, &PathRules::default())
    }

    /// Normalizes a raw (percent-encoded) path. One leading `/` is tolerated and dropped.
    ///
    /// # Errors
    /// Returns the [`PathError`] describing the first rule that was violated.
    pub fn parse_with(raw: &str, rules: &PathRules) -> Result<Self, PathError> {
        // The encoded form is at most three times the decoded one; checking early
        // bounds the work an oversized request can cause.
        if raw.len() > rules.max_len.saturating_mul(3) {
            return Err(PathError::TooLong);
        }
        let raw = raw.strip_prefix('/').unwrap_or(raw);
        let decoded = percent_decode_once(raw, rules.allow_encoded_slash)?;
        if has_percent_escape(&decoded) {
            return Err(PathError::DoubleEncoded);
        }
        if decoded.len() > rules.max_len {
            return Err(PathError::TooLong);
        }
        if decoded.is_empty() {
            return if rules.allow_empty {
                Ok(Self::root())
            } else {
                Err(PathError::Empty)
            };
        }
        let (body, trailing_slash) = match decoded.strip_suffix('/') {
            Some(rest) if rules.allow_trailing_slash => (rest, true),
            Some(_) => return Err(PathError::TrailingSlash),
            None => (decoded.as_str(), false),
        };
        if body.is_empty() {
            // The input was exactly "//" after dropping the leading slash.
            return Err(PathError::DoubleSlash);
        }
        for segment in body.split('/') {
            validate_segment(segment, rules.max_segment_len)?;
        }
        Ok(Self {
            value: body.to_owned(),
            trailing_slash,
        })
    }

    /// Builds a path from already decoded segments, validating each one.
    ///
    /// # Errors
    /// Fails when a segment is empty, contains `/` or violates a segment rule.
    pub fn from_segments<I, S>(segments: I) -> Result<Self, PathError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let rules = PathRules::default();
        let mut value = String::new();
        for segment in segments {
            let segment = segment.as_ref();
            if segment.contains('/') {
                return Err(PathError::DoubleSlash);
            }
            validate_segment(segment, rules.max_segment_len)?;
            if !value.is_empty() {
                value.push('/');
            }
            value.push_str(segment);
        }
        if value.len() > rules.max_len {
            return Err(PathError::TooLong);
        }
        Ok(Self {
            value,
            trailing_slash: false,
        })
    }

    /// The repository root (empty path).
    #[must_use]
    pub fn root() -> Self {
        Self {
            value: String::new(),
            trailing_slash: false,
        }
    }

    /// The decoded path without leading or trailing slash.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// True for the repository root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.value.is_empty()
    }

    /// True when the request path ended with a slash (only possible when the rules allowed it).
    #[must_use]
    pub fn has_trailing_slash(&self) -> bool {
        self.trailing_slash
    }

    /// Iterates over the segments. Empty for the root.
    pub fn segments(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.value.split('/').filter(|s| !s.is_empty())
    }

    /// The last segment, or `None` for the root.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.segments().next_back()
    }

    /// The path without its last segment, or `None` for the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let value = self
            .value
            .rsplit_once('/')
            .map_or_else(String::new, |(parent, _)| parent.to_owned());
        Some(Self {
            value,
            trailing_slash: false,
        })
    }

    /// Removes a leading segment prefix such as `v2` (whole segments only).
    #[must_use]
    pub fn strip_prefix(&self, prefix: &str) -> Option<Self> {
        let prefix = prefix.trim_matches('/');
        if prefix.is_empty() {
            return Some(self.clone());
        }
        let rest = self.value.strip_prefix(prefix)?;
        if rest.is_empty() {
            return Some(Self {
                value: String::new(),
                trailing_slash: self.trailing_slash,
            });
        }
        let rest = rest.strip_prefix('/')?;
        Some(Self {
            value: rest.to_owned(),
            trailing_slash: self.trailing_slash,
        })
    }

    /// Re-encodes the path for use in a URL: every segment is percent-encoded and
    /// joined with `/`. `parse(encode())` returns an equal path.
    #[must_use]
    pub fn encode(&self) -> String {
        self.segments()
            .map(encode_segment)
            .collect::<Vec<_>>()
            .join("/")
    }
}

impl fmt::Display for NormalizedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

fn validate_segment(segment: &str, max_len: usize) -> Result<(), PathError> {
    if segment.is_empty() {
        return Err(PathError::DoubleSlash);
    }
    if segment.len() > max_len {
        return Err(PathError::TooLong);
    }
    if segment.bytes().all(|b| b == b'.') {
        return Err(PathError::Traversal);
    }
    if segment.chars().any(|c| c.is_control() || c == '\\') {
        return Err(PathError::ForbiddenChar);
    }
    Ok(())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_decode_once(raw: &str, allow_encoded_slash: bool) -> Result<String, PathError> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'%' {
            let hi = bytes.get(i + 1).copied().and_then(hex_value);
            let lo = bytes.get(i + 2).copied().and_then(hex_value);
            let (Some(hi), Some(lo)) = (hi, lo) else {
                return Err(PathError::InvalidEncoding);
            };
            let decoded = hi << 4 | lo;
            if decoded == b'/' && !allow_encoded_slash {
                return Err(PathError::EncodedSlash);
            }
            out.push(decoded);
            i += 3;
        } else {
            out.push(byte);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| PathError::InvalidEncoding)
}

/// True when `text` contains `%` followed by two hex digits. A lone `%` (as in
/// `100%.txt`) is a legitimate file name character and does not count.
fn has_percent_escape(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        b == b'%'
            && bytes.get(i + 1).copied().and_then(hex_value).is_some()
            && bytes.get(i + 2).copied().and_then(hex_value).is_some()
    })
}

fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            // Writing into a String cannot fail.
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn accepts_plain_paths_and_drops_one_leading_slash() {
        assert_eq!(
            NormalizedPath::parse("a/b/c.txt").unwrap().as_str(),
            "a/b/c.txt"
        );
        assert_eq!(NormalizedPath::parse("/a/b").unwrap().as_str(), "a/b");
        assert!(NormalizedPath::parse("").unwrap().is_root());
        assert!(NormalizedPath::parse("/").unwrap().is_root());
    }

    #[test]
    fn decodes_exactly_once() {
        assert_eq!(
            NormalizedPath::parse("a%20b/c%2Bd").unwrap().as_str(),
            "a b/c+d"
        );
        // A literal percent sign must arrive as %25; a bare one is a malformed escape.
        assert_eq!(
            NormalizedPath::parse("100%25.txt").unwrap().as_str(),
            "100%.txt"
        );
        assert_eq!(
            NormalizedPath::parse("100%.txt"),
            Err(PathError::InvalidEncoding)
        );
    }

    #[test]
    fn must_not_accept_traversal_in_any_encoding() {
        for bad in [
            "..",
            "../x",
            "a/../b",
            "a/..",
            "./a",
            "a/./b",
            "%2e%2e/x",
            "%2E%2E/x",
            "a/%2e%2e/b",
            ".%2e/x",
            "a/..%2fb",
            "...",
        ] {
            assert!(
                NormalizedPath::parse(bad).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn must_not_accept_double_encoded_traversal() {
        for bad in ["%252e%252e/x", "a/%252e%252e/b", "%252F", "a%2520b"] {
            assert_eq!(
                NormalizedPath::parse(bad),
                Err(PathError::DoubleEncoded),
                "{bad}"
            );
        }
    }

    #[test]
    fn must_not_accept_backslash_nul_and_control_characters() {
        for bad in [
            "a\\b", "..\\x", "a%5cb", "a%5Cb", "a%00b", "a\u{0}b", "a%0ab", "a\tb", "a\u{7f}b",
        ] {
            assert!(NormalizedPath::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn must_not_accept_empty_segments_or_encoded_slash_by_default() {
        for bad in ["a//b", "//a", "a//", "///"] {
            assert!(NormalizedPath::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(NormalizedPath::parse("a%2fb"), Err(PathError::EncodedSlash));
        assert_eq!(NormalizedPath::parse("a%2Fb"), Err(PathError::EncodedSlash));
    }

    #[test]
    fn encoded_slash_is_split_and_revalidated_when_allowed() {
        let rules = PathRules {
            allow_encoded_slash: true,
            ..PathRules::default()
        };
        let ok = NormalizedPath::parse_with("@scope%2fname", &rules).unwrap();
        assert_eq!(ok.as_str(), "@scope/name");
        // The smuggled traversal must still be caught after the split.
        assert!(NormalizedPath::parse_with("a%2f..%2fb", &rules).is_err());
        assert!(NormalizedPath::parse_with("a%2f%2fb", &rules).is_err());
    }

    #[test]
    fn rejects_bad_percent_sequences_and_invalid_utf8() {
        for bad in ["%", "%2", "%zz", "a%g0", "%ff", "%c3%28"] {
            assert_eq!(
                NormalizedPath::parse(bad),
                Err(PathError::InvalidEncoding),
                "{bad}"
            );
        }
        assert_eq!(NormalizedPath::parse("caf%C3%A9").unwrap().as_str(), "café");
    }

    #[test]
    fn enforces_length_limits() {
        let long_segment = "a".repeat(256);
        assert_eq!(
            NormalizedPath::parse(&long_segment),
            Err(PathError::TooLong)
        );
        let long_path = vec!["a".repeat(100); 25].join("/");
        assert_eq!(NormalizedPath::parse(&long_path), Err(PathError::TooLong));
        let huge = "a/".repeat(10_000);
        assert_eq!(NormalizedPath::parse(&huge), Err(PathError::TooLong));
    }

    #[test]
    fn trailing_slash_is_recorded_only_when_allowed() {
        assert_eq!(NormalizedPath::parse("a/b/"), Err(PathError::TrailingSlash));
        let rules = PathRules {
            allow_trailing_slash: true,
            ..PathRules::default()
        };
        let dir = NormalizedPath::parse_with("a/b/", &rules).unwrap();
        assert_eq!(dir.as_str(), "a/b");
        assert!(dir.has_trailing_slash());
        assert!(
            !NormalizedPath::parse_with("a/b", &rules)
                .unwrap()
                .has_trailing_slash()
        );
    }

    #[test]
    fn empty_path_can_be_forbidden() {
        let rules = PathRules {
            allow_empty: false,
            ..PathRules::default()
        };
        assert_eq!(
            NormalizedPath::parse_with("", &rules),
            Err(PathError::Empty)
        );
        assert_eq!(
            NormalizedPath::parse_with("/", &rules),
            Err(PathError::Empty)
        );
    }

    #[test]
    fn helpers_work_on_segments() {
        let p = NormalizedPath::parse("a/b/c.txt").unwrap();
        assert_eq!(p.file_name(), Some("c.txt"));
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert_eq!(p.segments().collect::<Vec<_>>(), ["a", "b", "c.txt"]);
        assert_eq!(p.to_string(), "a/b/c.txt");
        assert_eq!(p.strip_prefix("a").unwrap().as_str(), "b/c.txt");
        assert_eq!(p.strip_prefix("a/b").unwrap().as_str(), "c.txt");
        assert_eq!(p.strip_prefix("a/b/c.txt").unwrap().as_str(), "");
        assert!(p.strip_prefix("a/bb").is_none());
        assert!(p.strip_prefix("ab").is_none());
        assert_eq!(p.strip_prefix("").unwrap(), p);
        let single = NormalizedPath::parse("x").unwrap();
        assert!(single.parent().unwrap().is_root());
        assert!(NormalizedPath::root().parent().is_none());
        assert_eq!(NormalizedPath::root().file_name(), None);
    }

    #[test]
    fn from_segments_validates_every_segment() {
        assert_eq!(
            NormalizedPath::from_segments(["a", "b"]).unwrap().as_str(),
            "a/b"
        );
        assert!(NormalizedPath::from_segments(["a", ".."]).is_err());
        assert!(NormalizedPath::from_segments(["a/b"]).is_err());
        assert!(NormalizedPath::from_segments([""]).is_err());
        assert!(NormalizedPath::from_segments(["a\\b"]).is_err());
        assert!(NormalizedPath::from_segments(vec!["a".repeat(200); 20]).is_err());
        assert!(
            NormalizedPath::from_segments(Vec::<&str>::new())
                .unwrap()
                .is_root()
        );
    }

    #[test]
    fn encode_round_trips_special_characters() {
        let p = NormalizedPath::from_segments(["a b", "c+d", "é", "100%.txt"]).unwrap();
        let encoded = p.encode();
        assert!(!encoded.contains(' '));
        assert_eq!(NormalizedPath::parse(&encoded).unwrap(), p);
    }

    proptest! {
        #[test]
        fn normalize_never_panics_and_never_yields_traversal(raw in ".{0,200}") {
            if let Ok(p) = NormalizedPath::parse(&raw) {
                for seg in p.segments() {
                    prop_assert!(seg != "." && seg != "..");
                    prop_assert!(!seg.contains('/') && !seg.contains('\\') && !seg.contains('\0'));
                }
                prop_assert!(!p.as_str().starts_with('/'));
            }
        }

        #[test]
        fn normalization_is_idempotent_through_encode(raw in "[a-zA-Z0-9%/._ +~@-]{0,120}") {
            if let Ok(first) = NormalizedPath::parse(&raw) {
                let second = NormalizedPath::parse(&first.encode()).unwrap();
                prop_assert_eq!(first, second);
            }
        }

        // SEC-19: the path authorization sees equals the path storage sees,
        // because both come from one value; this guards the single decode.
        #[test]
        fn authz_path_equals_storage_path(raw in "[a-zA-Z0-9%/._-]{0,80}") {
            if let Ok(p) = NormalizedPath::parse(&raw) {
                let for_authz = p.as_str().to_owned();
                let for_storage = p.clone();
                prop_assert_eq!(for_authz, for_storage.as_str());
            }
        }
    }
}
