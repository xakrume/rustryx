//! Proxy repositories: a cached, verified view of an upstream simple index.
//!
//! * A project page is the upstream document (JSON preferred, HTML accepted), cached by the proxy
//!   pipeline for the repository's metadata TTL, parsed here, and rewritten: every file URL points
//!   at this repository (`<origin>/packages/<project>/<file>`), never at the upstream, so a client
//!   cannot be sent to a host the SSRF policy did not vet.
//! * A file is looked up by name in that page. The URL the upstream lists for it, the hash
//!   it lists and the cache path (derived from the request, never from the document) decide the
//!   fetch: the body is verified against the listed hash before a byte is served, and a copy
//!   cached earlier whose digest differs from the hash now listed is evicted (the upstream
//!   replaced the file; serving the old bytes would make every client fail its own hash check).
//! * The upstream URL of a file must lie under the configured `remote_url` (same origin, below
//!   its path). That is the contract of the proxy pipeline's URL builder and is checked again
//!   here, so a hostile index cannot point the proxy elsewhere. Consequence, documented in
//!   `docs/format-pypi.md`: an index whose files live on another host (pypi.org serves them from
//!   `files.pythonhosted.org`) cannot be mirrored until the pipeline can address a second
//!   allow-listed origin.

use std::collections::BTreeMap;

use axum_core::body::Body;
use http::{HeaderValue, Response};
use rustryx_core::model::WriteOptions;
use rustryx_format_core::proxy::UpstreamPath;
use rustryx_format_core::{
    ArtifactRequest, CachedDocument, ExpectedChecksum, FormatContext, FormatError, HashAlgo,
    NormalizedPath, PathRules, ProxyFetch, UpstreamFailureKind,
};
use url::Url;

use crate::filename::DistFilename;
use crate::handler::PypiHandler;
use crate::hosted::component_spec;
use crate::metrics;
use crate::name::ProjectName;
use crate::page::{CoreMetadataRef, FileEntry, ParseError, ProjectPage, RootPage};
use crate::route::file_url;
use crate::{simple_html, simple_json};

/// What the proxy asks upstream for: JSON when it can, HTML when it must.
const UPSTREAM_ACCEPT: &str = "application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html;q=0.2, text/html;q=0.01";

fn blocked(detail: &str) -> FormatError {
    FormatError::upstream(UpstreamFailureKind::Blocked, detail.to_owned())
}

/// The URL of the upstream page of `name` (or the root when `name` is `None`), with the trailing
/// slash PEP 503 requires.
fn page_url(remote_url: &str, name: Option<&ProjectName>) -> Result<Url, FormatError> {
    let mut url = Url::parse(remote_url).map_err(|_| blocked("remote_url is not a valid url"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| blocked("remote_url cannot be used as a base"))?;
        segments.pop_if_empty().push("simple");
        if let Some(name) = name {
            segments.push(name.as_str());
        }
        segments.push("");
    }
    Ok(url)
}

fn trailing_slash_path(text: &str) -> Result<NormalizedPath, FormatError> {
    let rules = PathRules {
        allow_trailing_slash: true,
        ..PathRules::default()
    };
    NormalizedPath::parse_with(text, &rules).map_err(FormatError::from)
}

fn looks_like_json(content_type: &str, body: &[u8]) -> bool {
    content_type.to_ascii_lowercase().contains("json")
        || body
            .iter()
            .find(|b| !b.is_ascii_whitespace())
            .is_some_and(|b| *b == b'{')
}

fn parse_failure(error: ParseError) -> FormatError {
    match error {
        ParseError::TooMany => FormatError::IndexTooLarge {
            limit: crate::page::MAX_FILES_PER_PAGE as u64,
        },
        ParseError::Malformed => FormatError::upstream(
            UpstreamFailureKind::BadStatus,
            "the upstream index document could not be parsed",
        ),
    }
}

/// Makes every URL of a parsed page absolute against the page's own URL and drops what cannot
/// be a download (not `http` or `https`).
fn absolutize(page: &mut ProjectPage, base: &Url) {
    page.files.retain_mut(|entry| match base.join(&entry.url) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => {
            entry.url = url.to_string();
            true
        }
        _ => false,
    });
}

/// The page as the client sees it: this repository's URLs, only files that belong to the
/// project and that this repository can serve.
pub(crate) fn client_page(
    upstream: &ProjectPage,
    name: &ProjectName,
    origin_url: &str,
) -> ProjectPage {
    let mut page = ProjectPage {
        name: name.to_string(),
        files: upstream
            .files
            .iter()
            .filter(|entry| {
                DistFilename::parse(&entry.filename).is_ok_and(|dist| dist.project() == name)
            })
            .map(|entry| FileEntry {
                url: file_url(origin_url, name, &entry.filename),
                ..entry.clone()
            })
            .collect(),
    };
    // The same file listed twice (an index that lists mirrors) is one file: the first wins.
    let mut seen = std::collections::HashSet::new();
    page.files
        .retain(|entry| seen.insert(entry.filename.clone()));
    page.sort_files();
    page
}

/// Digest the upstream lists for a file, as an expectation the proxy pipeline verifies before
/// serving. SHA-256 first; algorithms the pipeline cannot compute give no expectation.
fn expected_from(hashes: &BTreeMap<String, String>) -> Option<ExpectedChecksum> {
    [
        ("sha256", HashAlgo::Sha256),
        ("sha1", HashAlgo::Sha1),
        ("md5", HashAlgo::Md5),
    ]
    .into_iter()
    .find_map(|(name, algo)| {
        hashes
            .get(name)
            .and_then(|hex| ExpectedChecksum::new(algo, hex).ok())
    })
}

/// The part of `file_url` below `remote`, and its query, when the URL is on the upstream origin
/// and below its path prefix.
fn upstream_target(file_url: &str, remote: &Url) -> Option<(String, Option<String>)> {
    let url = Url::parse(file_url).ok()?;
    let same_origin = url.scheme() == remote.scheme()
        && url.host_str() == remote.host_str()
        && url.port_or_known_default() == remote.port_or_known_default();
    if !same_origin || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let base = remote.path().trim_end_matches('/');
    let rest = url.path().strip_prefix(base)?;
    let relative = rest.strip_prefix('/')?;
    (!relative.is_empty()).then(|| (relative.to_owned(), url.query().map(str::to_owned)))
}

impl PypiHandler {
    fn upstream_fetch<'a>(
        req: &'a ArtifactRequest,
        path: &NormalizedPath,
        cache_path: &str,
    ) -> ProxyFetch<'a> {
        ProxyFetch::new(req)
            .upstream_path(UpstreamPath::Decoded(path.clone()))
            .accept(HeaderValue::from_static(UPSTREAM_ACCEPT))
            .cache_path(cache_path)
    }

    /// The upstream project page, cached by the pipeline, parsed on the blocking pool. The URLs in
    /// the result are the upstream's, absolute.
    pub(crate) async fn proxy_upstream_page(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
    ) -> Result<(ProjectPage, CachedDocument), FormatError> {
        let remote = cx
            .repository()
            .proxy
            .as_ref()
            .map(|config| config.remote_url.clone())
            .ok_or_else(|| FormatError::internal("proxy repository without proxy settings"))?;
        let base = page_url(&remote, Some(name))?;
        let path = trailing_slash_path(&format!("simple/{name}/"))?;
        let fetch = Self::upstream_fetch(req, &path, &format!("simple/{name}"));
        let document = cx.proxy().fetch_document(fetch).await?;
        let (body, content_type) = (document.body.clone(), document.content_type.clone());
        let project = name.to_string();
        let page = self
            .blocking
            .run(move || {
                let mut page = if looks_like_json(&content_type, &body) {
                    simple_json::parse_project(&body, &project)
                } else {
                    simple_html::parse_project(&body, Some(&base), &project)
                }
                .map_err(parse_failure)?;
                absolutize(&mut page, &base);
                Ok(page)
            })
            .await?;
        Ok((page, document))
    }

    /// The upstream root index.
    pub(crate) async fn proxy_root(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
    ) -> Result<(RootPage, CachedDocument), FormatError> {
        let path = trailing_slash_path("simple/")?;
        let fetch = Self::upstream_fetch(req, &path, "simple");
        let document = cx.proxy().fetch_document(fetch).await?;
        let (body, content_type) = (document.body.clone(), document.content_type.clone());
        let root = self
            .blocking
            .run(move || {
                if looks_like_json(&content_type, &body) {
                    simple_json::parse_root(&body)
                } else {
                    simple_html::parse_root(&body)
                }
                .map_err(parse_failure)
            })
            .await?;
        Ok((root, document))
    }

    /// Serves a distribution of the upstream, verified against the hash its index lists.
    pub(crate) async fn proxy_file(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
        file: &DistFilename,
    ) -> Result<Response<Body>, FormatError> {
        let entry = self.listed_file(cx, req, name, file).await?;
        let (target, query, expected) = Self::fetch_plan(cx, &entry, false)?;
        if let Some(expected) = &expected {
            evict_if_replaced(cx, req.path.as_str(), expected).await?;
        }
        let mut fetch = ProxyFetch::new(req)
            .upstream_path(UpstreamPath::Encoded(target))
            .cache_path(req.path.as_str())
            .component(component_spec(name, file));
        if let Some(query) = query {
            fetch = fetch.upstream_query(query);
        }
        if let Some(expected) = expected {
            fetch = fetch.expect(expected);
        }
        cx.proxy().fetch_artifact(fetch).await
    }

    /// Serves the PEP 658 metadata file of a listed distribution.
    pub(crate) async fn proxy_metadata(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
        file: &DistFilename,
    ) -> Result<Response<Body>, FormatError> {
        let entry = self.listed_file(cx, req, name, file).await?;
        let Some(reference) = entry.core_metadata.clone() else {
            // The upstream does not offer a metadata file for it: there is none to serve.
            return Err(FormatError::NotFound);
        };
        let (target, query, _) = Self::fetch_plan(cx, &entry, true)?;
        let mut fetch = ProxyFetch::new(req)
            .upstream_path(UpstreamPath::Encoded(target))
            .cache_path(req.path.as_str());
        if let Some(query) = query {
            fetch = fetch.upstream_query(query);
        }
        if let CoreMetadataRef::Hashes(hashes) = &reference
            && let Some(expected) = expected_from(hashes)
        {
            fetch = fetch.expect(expected);
        }
        cx.proxy().fetch_artifact(fetch).await
    }

    async fn listed_file(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
        file: &DistFilename,
    ) -> Result<FileEntry, FormatError> {
        let (page, _) = self.proxy_upstream_page(cx, req, name).await?;
        page.files
            .into_iter()
            .find(|entry| entry.filename == file.as_str())
            .ok_or(FormatError::NotFound)
    }

    /// Where to fetch the entry and what to verify it against.
    fn fetch_plan(
        cx: &FormatContext,
        entry: &FileEntry,
        metadata: bool,
    ) -> Result<(String, Option<String>, Option<ExpectedChecksum>), FormatError> {
        let remote = cx
            .repository()
            .proxy
            .as_ref()
            .and_then(|config| Url::parse(&config.remote_url).ok())
            .ok_or_else(|| blocked("remote_url is not a valid url"))?;
        let Some((mut target, query)) = upstream_target(&entry.url, &remote) else {
            metrics::proxy_unreachable();
            return Err(blocked(
                "the file is hosted outside the configured remote_url",
            ));
        };
        if metadata {
            target.push_str(".metadata");
        }
        Ok((target, query, expected_from(&entry.hashes)))
    }
}

/// Drops a cached copy whose digest is not the one the upstream lists now.
async fn evict_if_replaced(
    cx: &FormatContext,
    cache_path: &str,
    expected: &ExpectedChecksum,
) -> Result<(), FormatError> {
    let repository = cx.repository();
    let Some(cached) = cx
        .ports()
        .assets
        .get_by_path(repository.id, cache_path)
        .await?
    else {
        return Ok(());
    };
    if expected.matches(&cached.checksums) {
        return Ok(());
    }
    // A cache entry is the system's doing, not a client's write: it is removed by the system,
    // not through the write path that a proxy refuses.
    cx.ports()
        .assets
        .delete_by_path_checked(repository.id, cache_path, &WriteOptions::new())
        .await?;
    metrics::proxy_evicted();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> ProjectName {
        ProjectName::parse(text).unwrap()
    }

    #[test]
    fn page_urls_keep_the_prefix_and_end_with_a_slash() {
        assert_eq!(
            page_url("https://up.test", Some(&name("foo")))
                .unwrap()
                .as_str(),
            "https://up.test/simple/foo/"
        );
        assert_eq!(
            page_url("https://up.test/pypi/", Some(&name("foo-bar")))
                .unwrap()
                .as_str(),
            "https://up.test/pypi/simple/foo-bar/"
        );
        assert_eq!(
            page_url("https://up.test/pypi", None).unwrap().as_str(),
            "https://up.test/pypi/simple/"
        );
        assert!(page_url("not a url", None).is_err());
        assert!(page_url("mailto:x@y", None).is_err());
    }

    #[test]
    fn upstream_targets_must_stay_below_remote_url() {
        let remote = Url::parse("https://up.test/pypi/").unwrap();
        assert_eq!(
            upstream_target(
                "https://up.test/pypi/packages/ab/foo-1.0.tar.gz?x=1",
                &remote
            ),
            Some((
                "packages/ab/foo-1.0.tar.gz".to_owned(),
                Some("x=1".to_owned())
            ))
        );
        for outside in [
            "https://files.test/pypi/packages/foo-1.0.tar.gz",
            "https://up.test/other/foo-1.0.tar.gz",
            "https://up.test/pypi",
            "https://up.test/pypi/",
            "https://up.test/pypiX/foo-1.0.tar.gz",
            "http://up.test/pypi/foo-1.0.tar.gz",
            "https://u:p@up.test/pypi/foo-1.0.tar.gz",
            "https://up.test:8443/pypi/foo-1.0.tar.gz",
            "not a url",
        ] {
            assert_eq!(upstream_target(outside, &remote), None, "{outside}");
        }
        let root = Url::parse("https://up.test/").unwrap();
        assert_eq!(
            upstream_target("https://up.test/files/f-1.tar.gz", &root)
                .unwrap()
                .0,
            "files/f-1.tar.gz"
        );
    }

    #[test]
    fn expected_digests_prefer_sha256_and_skip_unknown_algorithms() {
        let hashes: BTreeMap<String, String> = [
            ("md5".to_owned(), "00".repeat(16)),
            ("sha256".to_owned(), "ab".repeat(32)),
            ("sha512".to_owned(), "cd".repeat(64)),
        ]
        .into_iter()
        .collect();
        let expected = expected_from(&hashes).unwrap();
        assert_eq!(expected.algo(), HashAlgo::Sha256);
        let only_sha512: BTreeMap<String, String> = [("sha512".to_owned(), "cd".repeat(64))]
            .into_iter()
            .collect();
        assert!(expected_from(&only_sha512).is_none());
        let bad: BTreeMap<String, String> = [("sha256".to_owned(), "abc".to_owned())]
            .into_iter()
            .collect();
        assert!(expected_from(&bad).is_none());
    }

    #[test]
    fn the_client_page_points_at_this_repository_and_keeps_only_the_project() {
        let upstream = ProjectPage {
            name: "foo".into(),
            files: vec![
                FileEntry::new("foo-1.0.tar.gz", "https://up.test/x/foo-1.0.tar.gz"),
                FileEntry::new("foo-1.0.tar.gz", "https://mirror.test/foo-1.0.tar.gz"),
                FileEntry::new("bar-1.0.tar.gz", "https://up.test/x/bar-1.0.tar.gz"),
                FileEntry::new("foo-1.0.egg", "https://up.test/x/foo-1.0.egg"),
                FileEntry::new("Foo_-0.9-py3-none-any.whl", "https://up.test/x/w"),
                FileEntry::new(
                    "foo-0.9-py3-none-any.whl",
                    "https://up.test/x/foo-0.9-py3-none-any.whl",
                ),
            ],
        };
        let page = client_page(&upstream, &name("foo"), "https://r.test/repository/p");
        let names: Vec<_> = page.files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["foo-0.9-py3-none-any.whl", "foo-1.0.tar.gz"]);
        assert!(page.files.iter().all(|f| {
            f.url
                .starts_with("https://r.test/repository/p/packages/foo/")
        }));
        assert_eq!(
            page.files[1].url,
            "https://r.test/repository/p/packages/foo/foo-1.0.tar.gz"
        );
    }

    #[test]
    fn relative_urls_are_resolved_and_non_http_ones_dropped() {
        let base = Url::parse("https://up.test/simple/foo/").unwrap();
        let mut page = ProjectPage {
            name: "foo".into(),
            files: vec![
                FileEntry::new("a-1.tar.gz", "../../packages/a-1.tar.gz"),
                FileEntry::new("b-1.tar.gz", "https://cdn.test/b-1.tar.gz"),
                FileEntry::new("c-1.tar.gz", "javascript:alert(1)"),
                FileEntry::new("d-1.tar.gz", "ftp://h/d"),
            ],
        };
        absolutize(&mut page, &base);
        let urls: Vec<_> = page.files.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://up.test/packages/a-1.tar.gz",
                "https://cdn.test/b-1.tar.gz"
            ]
        );
    }

    #[test]
    fn json_is_recognized_by_type_or_by_its_first_byte() {
        assert!(looks_like_json(
            "application/vnd.pypi.simple.v1+json",
            b"<html>"
        ));
        assert!(looks_like_json("text/html", b"  {\"files\":[]}"));
        assert!(!looks_like_json("text/html", b"<html>"));
        assert!(!looks_like_json("", b""));
    }

    #[test]
    fn parse_failures_map_to_the_right_errors() {
        assert!(matches!(
            parse_failure(ParseError::TooMany),
            FormatError::IndexTooLarge { .. }
        ));
        assert!(matches!(
            parse_failure(ParseError::Malformed),
            FormatError::Upstream { .. }
        ));
    }
}
