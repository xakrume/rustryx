//! The path a content selector sees.
//!
//! Format crates normalize and validate the request path once (SEC-19) and
//! hand the same string to authorization and to storage. This module is the
//! second line of defense for selectors: a selector such as
//! `path.startsWith("/public/")` must never match `/public/../secret/x`, even if
//! a format crate has a bug, so a path that is not canonical never matches a
//! selector privilege. Repository wide privileges do not look at the path, so
//! they are unaffected.

use std::borrow::Cow;

/// Returns the selector view of `path`: exactly one leading `/` added when it
/// is missing. Returns `None` when the path is not canonical, which means:
///
/// - a `.` or `..` segment,
/// - an empty segment other than a single trailing one (`//`),
/// - a NUL or other control character.
///
/// Percent-encoding is not interpreted here: the formats decode before they
/// classify, and rbac must see the exact string storage will use.
#[must_use]
pub fn selector_path(path: &str) -> Option<Cow<'_, str>> {
    if path.chars().any(char::is_control) {
        return None;
    }
    let rest = path.strip_prefix('/').unwrap_or(path);
    let mut segments = rest.split('/').peekable();
    while let Some(segment) = segments.next() {
        let last = segments.peek().is_none();
        match segment {
            "." | ".." => return None,
            "" if !last => return None,
            _ => {}
        }
    }
    if path.starts_with('/') {
        Some(Cow::Borrowed(path))
    } else {
        Some(Cow::Owned(format!("/{path}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_one_leading_slash() {
        assert_eq!(
            selector_path("org/example/a.jar").unwrap(),
            "/org/example/a.jar"
        );
        assert_eq!(
            selector_path("/org/example/a.jar").unwrap(),
            "/org/example/a.jar"
        );
        assert!(matches!(selector_path("/a").unwrap(), Cow::Borrowed(_)));
    }

    #[test]
    fn root_and_directory_paths_are_canonical() {
        assert_eq!(selector_path("").unwrap(), "/");
        assert_eq!(selector_path("/").unwrap(), "/");
        assert_eq!(selector_path("org/example/").unwrap(), "/org/example/");
    }

    #[test]
    fn rejects_traversal_and_empty_segments() {
        for bad in [
            "..",
            "/..",
            "a/../b",
            "/public/../secret/x",
            "a/./b",
            "./a",
            "a/..",
            "//",
            "//a",
            "a//b",
            "/a//b",
            "a//",
        ] {
            assert!(selector_path(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn rejects_control_characters() {
        for bad in ["a\0b", "a\nb", "a\tb", "\u{7f}"] {
            assert!(selector_path(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn dots_inside_names_are_fine() {
        assert_eq!(selector_path("a/..b/c.../.d").unwrap(), "/a/..b/c.../.d");
        assert_eq!(selector_path("%2e%2e/x").unwrap(), "/%2e%2e/x");
    }
}
