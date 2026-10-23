//! Building the upstream URL from the repository's `remote_url` and a request path.
//!
//! Invariant: the result has the scheme, host and port of `remote_url` and a path
//! below its base path. Whatever the request path contained, a proxy request can
//! only ever reach the configured upstream, below the configured prefix. The
//! invariant is checked on the finished URL, not assumed from the construction.

use url::Url;

use crate::error::{FormatError, UpstreamFailureKind};
use crate::path::{NormalizedPath, PathRules};

/// The part of the upstream URL below `remote_url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamPath {
    /// A normalized path whose segments are percent-encoded when the URL is built.
    Decoded(NormalizedPath),
    /// A path that is already percent-encoded and must reach the upstream verbatim
    /// (npm sends scoped packages as `@scope%2fname`). It is validated with the
    /// same rules as any request path, so `..` in any encoding is still refused.
    Encoded(String),
}

impl From<NormalizedPath> for UpstreamPath {
    fn from(path: NormalizedPath) -> Self {
        Self::Decoded(path)
    }
}

fn invalid(detail: &str) -> FormatError {
    FormatError::upstream(UpstreamFailureKind::Blocked, detail.to_owned())
}

/// Joins `base` and `path` into the URL to request. `query` is appended verbatim
/// (percent-encoded by the caller) when present.
///
/// # Errors
/// A [`FormatError::Upstream`] with kind `Blocked` when the path is invalid or the
/// result would leave the base URL's origin or path prefix.
pub(crate) fn build_upstream_url(
    base: &Url,
    path: &UpstreamPath,
    query: Option<&str>,
) -> Result<Url, FormatError> {
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    let base_path = base.path().trim_end_matches('/').to_owned();
    match path {
        UpstreamPath::Decoded(normalized) => {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| invalid("remote_url cannot be used as a base"))?;
            segments.pop_if_empty();
            segments.extend(normalized.segments());
            if normalized.has_trailing_slash() {
                segments.push("");
            }
        }
        UpstreamPath::Encoded(raw) => {
            let rules = PathRules {
                allow_encoded_slash: true,
                allow_trailing_slash: true,
                ..PathRules::default()
            };
            NormalizedPath::parse_with(raw, &rules)
                .map_err(|_| invalid("invalid upstream path"))?;
            url.set_path(&format!("{base_path}/{}", raw.trim_start_matches('/')));
        }
    }
    url.set_query(query);
    let same_origin = url.scheme() == base.scheme()
        && url.host_str() == base.host_str()
        && url.port_or_known_default() == base.port_or_known_default();
    let below_base = base_path.is_empty()
        || url.path() == base_path
        || url.path().starts_with(&format!("{base_path}/"));
    if same_origin && below_base && url.username().is_empty() && url.password().is_none() {
        Ok(url)
    } else {
        Err(invalid("upstream url left the configured remote_url"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn decoded(path: &str) -> UpstreamPath {
        UpstreamPath::Decoded(NormalizedPath::parse(path).unwrap())
    }

    #[test]
    fn joins_below_the_base_path() {
        let url = build_upstream_url(
            &base("https://repo1.maven.org/maven2/"),
            &decoded("a/b/c.jar"),
            None,
        )
        .unwrap();
        assert_eq!(url.as_str(), "https://repo1.maven.org/maven2/a/b/c.jar");
        let url = build_upstream_url(
            &base("https://registry.npmjs.org"),
            &decoded("lodash"),
            None,
        )
        .unwrap();
        assert_eq!(url.as_str(), "https://registry.npmjs.org/lodash");
    }

    #[test]
    fn encodes_segments_and_keeps_unusual_characters_harmless() {
        let url =
            build_upstream_url(&base("https://h.example/r/"), &decoded("a b/c+d/é"), None).unwrap();
        assert_eq!(url.path(), "/r/a%20b/c+d/%C3%A9");
        // A first segment that looks like a scheme must stay a path segment.
        let url =
            build_upstream_url(&base("https://h.example/r/"), &decoded("evil:x/y"), None).unwrap();
        assert_eq!(url.host_str(), Some("h.example"));
        assert_eq!(url.path(), "/r/evil:x/y");
        let url = build_upstream_url(&base("https://h.example/r/"), &decoded("a?x=1#frag"), None)
            .unwrap();
        assert_eq!(url.path(), "/r/a%3Fx=1%23frag");
        assert_eq!(url.query(), None);
    }

    #[test]
    fn trailing_slash_and_query_are_preserved_when_asked() {
        let rules = PathRules {
            allow_trailing_slash: true,
            ..PathRules::default()
        };
        let dir = NormalizedPath::parse_with("a/b/", &rules).unwrap();
        let url = build_upstream_url(
            &base("https://h.example/r"),
            &UpstreamPath::Decoded(dir),
            Some("x=1&y=2"),
        )
        .unwrap();
        assert_eq!(url.as_str(), "https://h.example/r/a/b/?x=1&y=2");
    }

    #[test]
    fn must_not_leave_the_configured_origin_or_prefix() {
        for raw in [
            "../other",
            "a/../../x",
            "%2e%2e/x",
            "a/%2e%2e%2f%2e%2e/x",
            "..",
            "a\\b",
            "//evil.example/x",
        ] {
            let result = build_upstream_url(
                &base("https://h.example/repo1/"),
                &UpstreamPath::Encoded(raw.to_owned()),
                None,
            );
            assert!(result.is_err(), "{raw} must be refused");
        }
    }

    #[test]
    fn encoded_paths_reach_the_upstream_verbatim_after_validation() {
        let url = build_upstream_url(
            &base("https://registry.npmjs.org/"),
            &UpstreamPath::Encoded("@scope%2fname".to_owned()),
            None,
        )
        .unwrap();
        assert_eq!(url.as_str(), "https://registry.npmjs.org/@scope%2fname");
        let url = build_upstream_url(
            &base("https://h.example/prefix"),
            &UpstreamPath::Encoded("/a/b%20c".to_owned()),
            None,
        )
        .unwrap();
        assert_eq!(url.path(), "/prefix/a/b%20c");
    }

    #[test]
    fn a_remote_url_with_userinfo_is_refused() {
        let result = build_upstream_url(&base("https://user:pw@h.example/r/"), &decoded("a"), None);
        assert!(result.is_err());
    }

    #[test]
    fn from_normalized_path() {
        let path = NormalizedPath::parse("a/b").unwrap();
        assert_eq!(
            UpstreamPath::from(path.clone()),
            UpstreamPath::Decoded(path)
        );
    }
}
