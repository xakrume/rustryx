//! What a request path of a raw repository addresses: a file or a directory.
//!
//! The trailing slash is the only thing that decides. `GET a/b` addresses the file `a/b` and
//! nothing else; `GET a/b/` addresses the directory `a/b` and nothing else; there is no
//! redirect and no fallback from one to the other. Two layers that disagree on whether `a/b`
//! may also mean `a/b/` are two layers that can be made to authorize one thing and serve
//! another, so the ambiguity is removed instead of repaired (SEC-19, SEC-31).
//!
//! The path is normalized once, here, with the strict rules of `rustryx-format-core`
//! (`..` in any encoding, backslash, NUL and control characters, `//`, double encoding,
//! encoded slashes) plus two raw specific ones: a depth limit, which bounds the ancestor
//! lookups of an upload, and a ban on invisible and bidirectional control characters, which
//! would let a file name read differently in a listing than it is stored.

use rustryx_format_core::{FormatError, NormalizedPath, PathRules};

/// Deepest path accepted, in segments. An upload checks every ancestor for being a file
/// (one lookup each), so the depth bounds that work.
pub const MAX_DEPTH: usize = 32;

/// Longest decoded path, in bytes.
pub const MAX_PATH_LEN: usize = 1024;

/// Longest decoded segment, in bytes (the limit of most file systems).
pub const MAX_SEGMENT_LEN: usize = 255;

/// What a request path addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A directory prefix: the repository root, or a path that ended with a slash.
    Directory(NormalizedPath),
    /// A file.
    File(NormalizedPath),
}

impl Target {
    /// The normalized path, without a trailing slash.
    #[must_use]
    pub fn path(&self) -> &NormalizedPath {
        match self {
            Self::Directory(path) | Self::File(path) => path,
        }
    }

    /// True for a directory.
    #[must_use]
    pub fn is_directory(&self) -> bool {
        matches!(self, Self::Directory(_))
    }
}

/// Normalizes the raw (percent-encoded) path below the repository root.
///
/// # Errors
/// An opaque `BadRequest` for anything the rules refuse; the reason is not echoed, because it
/// would tell an attacker which variant of a traversal got past a filter in front of us.
pub fn parse_target(raw: &str) -> Result<Target, FormatError> {
    let rules = PathRules {
        max_len: MAX_PATH_LEN,
        max_segment_len: MAX_SEGMENT_LEN,
        allow_trailing_slash: true,
        allow_encoded_slash: false,
        allow_empty: true,
    };
    let path = NormalizedPath::parse_with(raw, &rules)?;
    if path.segments().count() > MAX_DEPTH {
        return Err(FormatError::bad_request("path is too deep"));
    }
    if path.as_str().chars().any(is_spoofing_char) {
        return Err(FormatError::bad_request("invalid path"));
    }
    if path.is_root() || path.has_trailing_slash() {
        Ok(Target::Directory(path))
    } else {
        Ok(Target::File(path))
    }
}

/// Characters that are invisible or reorder text: zero width characters, the bidirectional
/// controls (the "trojan source" family), the soft hyphen, the byte order mark and the
/// Arabic letter mark. None of them belongs in a file name.
fn is_spoofing_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn bad(raw: &str) -> bool {
        matches!(parse_target(raw), Err(FormatError::BadRequest(_)))
    }

    #[test]
    fn the_trailing_slash_decides_between_file_and_directory() {
        assert!(matches!(parse_target("a/b").unwrap(), Target::File(_)));
        assert!(matches!(
            parse_target("a/b/").unwrap(),
            Target::Directory(_)
        ));
        assert!(parse_target("").unwrap().is_directory());
        assert!(parse_target("/").unwrap().is_directory());
        assert_eq!(parse_target("a/b/").unwrap().path().as_str(), "a/b");
    }

    #[test]
    fn must_not_accept_traversal_in_any_form() {
        for raw in [
            "..",
            "../x",
            "a/../b",
            "a/..",
            "a/../",
            "%2e%2e/x",
            "%2E%2E%2Fx",
            "a/%2e%2e/b",
            ".%2e/x",
            "a/..%2fb",
            "..%5cx",
            "a%5c..%5cb",
            "%252e%252e/x",
            "./a",
            "a/./b",
            "...",
        ] {
            assert!(bad(raw), "{raw} must be refused");
        }
    }

    #[test]
    fn must_not_accept_backslash_nul_control_and_double_slash() {
        for raw in [
            "a\\b", "a%5Cb", "a%00b", "a\u{0}b", "a%0Ab", "a%0d%0ab", "a//b", "//a", "a%2Fb",
            "a%2fb",
        ] {
            assert!(bad(raw), "{raw:?} must be refused");
        }
    }

    #[test]
    fn must_not_accept_overlong_paths_and_segments() {
        assert!(bad(&"a".repeat(MAX_SEGMENT_LEN + 1)));
        assert!(parse_target(&"a".repeat(MAX_SEGMENT_LEN)).is_ok());
        let long = vec!["a".repeat(100); 11].join("/");
        assert!(bad(&long), "over {MAX_PATH_LEN} bytes");
        let deep = vec!["a"; MAX_DEPTH + 1].join("/");
        assert!(bad(&deep));
        let ok = vec!["a"; MAX_DEPTH].join("/");
        assert!(parse_target(&ok).is_ok());
    }

    #[test]
    fn must_not_accept_invisible_and_bidirectional_characters() {
        for raw in [
            "a%E2%80%AEb",
            "a%E2%80%8Bb",
            "a%EF%BB%BFb",
            "a%C2%ADb",
            "a%E2%81%A6b",
        ] {
            assert!(bad(raw), "{raw} must be refused");
        }
        assert_eq!(
            parse_target("caf%C3%A9.txt").unwrap().path().as_str(),
            "café.txt"
        );
    }

    #[test]
    fn a_failure_does_not_say_which_rule_it_was() {
        for raw in ["../x", "a//b", "a%5Cb", "%252e%252e"] {
            let Err(FormatError::BadRequest(message)) = parse_target(raw) else {
                panic!("{raw} must be refused");
            };
            assert_eq!(message, "invalid path");
        }
    }

    proptest! {
        #[test]
        fn parsing_never_panics_and_never_yields_a_traversal(raw in ".{0,300}") {
            if let Ok(target) = parse_target(&raw) {
                let path = target.path();
                for segment in path.segments() {
                    prop_assert!(segment != "." && segment != "..");
                    prop_assert!(!segment.contains(['/', '\\', '\0']));
                    prop_assert!(!segment.chars().any(char::is_control));
                    prop_assert!(!segment.chars().any(is_spoofing_char));
                }
                prop_assert!(path.segments().count() <= MAX_DEPTH);
            }
        }

        #[test]
        fn parsing_is_idempotent_through_encode(raw in "[a-zA-Z0-9%/._ +~@-]{0,100}") {
            if let Ok(first) = parse_target(&raw) {
                let mut encoded = first.path().encode();
                if first.is_directory() && !first.path().is_root() {
                    encoded.push('/');
                }
                prop_assert_eq!(parse_target(&encoded).unwrap(), first);
            }
        }

        #[test]
        fn a_file_is_never_a_directory_and_the_other_way_round(raw in "[a-z/]{0,40}") {
            if let Ok(target) = parse_target(&raw) {
                let ends_with_slash = raw.ends_with('/') || raw.is_empty();
                prop_assert_eq!(target.is_directory(), ends_with_slash);
            }
        }
    }
}
