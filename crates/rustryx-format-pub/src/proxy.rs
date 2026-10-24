//! A proxy repository over a hosted pub repository (by default `https://pub.dev`).
//!
//! Three documents are served, and they depend on each other:
//!
//! * **The listing** is fetched from `<remote>/api/packages/<package>` with
//!   `Accept: application/vnd.pub.v2+json` through format-core's cached document path (metadata
//!   TTL, singleflight, negative cache, stale serving, SSRF guard, retries). The cached bytes are
//!   the upstream's own. Every request reads them and builds the client's document from them
//!   ([`PackageDoc`]): `archive_url` is rewritten to `<repository>/packages/<package>/versions/
//!   <version>.tar.gz` under the repository the client addressed (for a group, the group), and
//!   entries that cannot be trusted are dropped. The rewrite is done at read time, not stored,
//!   because it depends on the address the client used.
//! * **The single version document** (deprecated endpoint) is derived from that listing, so the
//!   proxy keeps one cached document per package and the two can never disagree.
//! * **The archive** is fetched from `<remote>/packages/<package>/versions/<version>.tar.gz`, the
//!   deprecated archive path that the specification requires servers to keep, which the public
//!   registry answers with a redirect to the file. The executor follows up to three redirects and
//!   validates each hop against the SSRF policy, so the host of the file must be allowed there
//!   (see `docs/format-pub.md`). The archive is fetched only for a version the listing contains,
//!   and with the listing's `archive_sha256` as the expected checksum: format-core then writes it
//!   to the blob store, compares the digest of the real bytes, publishes it, and only then serves
//!   it. A mismatch is a 502 and nothing is cached or sent (no tee). A version without a digest in
//!   the listing is served unverified, and its entry carries no digest either, so the client
//!   computes its own.
//!
//! The upstream's `401` and `403` never reach the client (format-core maps them to 502).

use axum_core::body::Body;
use http::{HeaderValue, Response};
use rustryx_format_core::{
    ArtifactRequest, CacheStatus, ComponentSpec, ExpectedChecksum, FormatContext, FormatError,
    HashAlgo, NormalizedPath, ProxyFetch, UpstreamFailureKind, UpstreamPath,
};

use crate::blocking::BlockingPool;
use crate::config::PubConfig;
use crate::document::{ArchiveUrls, PackageDoc};
use crate::error::MEDIA_TYPE;
use crate::metrics;
use crate::name::PackageName;
use crate::response;
use crate::route::{archive_path, list_path};
use crate::version::PubVersion;

/// `GET api/packages/<package>`.
pub(crate) async fn list(
    cx: &FormatContext,
    cfg: &PubConfig,
    pool: &BlockingPool,
    req: &ArtifactRequest,
    name: &PackageName,
) -> Result<Response<Body>, FormatError> {
    let (doc, cache) = upstream_listing(cx, cfg, pool, req, name).await?;
    let mut response = response::document(req, doc.to_bytes());
    response::mark_cache(&mut response, cache);
    Ok(response)
}

/// `GET api/packages/<package>/versions/<version>`.
pub(crate) async fn version(
    cx: &FormatContext,
    cfg: &PubConfig,
    pool: &BlockingPool,
    req: &ArtifactRequest,
    name: &PackageName,
    version: &PubVersion,
) -> Result<Response<Body>, FormatError> {
    let (doc, cache) = upstream_listing(cx, cfg, pool, req, name).await?;
    let entry = doc.find(version).ok_or(FormatError::NotFound)?;
    let mut response = response::document(req, PackageDoc::single_version_bytes(entry));
    response::mark_cache(&mut response, cache);
    Ok(response)
}

/// `GET packages/<package>/versions/<version>.tar.gz`.
pub(crate) async fn archive(
    cx: &FormatContext,
    cfg: &PubConfig,
    pool: &BlockingPool,
    req: &ArtifactRequest,
    name: &PackageName,
    version: &PubVersion,
) -> Result<Response<Body>, FormatError> {
    // Only a version the upstream lists is fetched, and the listing says what its bytes hash to.
    let (doc, _) = upstream_listing(cx, cfg, pool, req, name).await?;
    let entry = doc.find(version).ok_or(FormatError::NotFound)?;
    let mut fetch = ProxyFetch::new(req)
        .content_type("application/gzip")
        .component(ComponentSpec {
            group: None,
            name: name.to_string(),
            version: Some(version.to_string()),
        });
    if let Some(sha256) = &entry.archive_sha256 {
        let expected = ExpectedChecksum::new(HashAlgo::Sha256, sha256)
            .map_err(|_| FormatError::internal("a listing entry holds an invalid digest"))?;
        fetch = fetch.expect(expected);
    }
    cx.proxy().fetch_artifact(fetch).await
}

/// The upstream listing of a package, read and rewritten for the client.
async fn upstream_listing(
    cx: &FormatContext,
    cfg: &PubConfig,
    pool: &BlockingPool,
    req: &ArtifactRequest,
    name: &PackageName,
) -> Result<(PackageDoc, CacheStatus), FormatError> {
    let path = NormalizedPath::parse(&list_path(name))?;
    // The cache path and the upstream path are derived from the request, never from upstream
    // data, and the same for the three routes: one cached document per package.
    let fetch = ProxyFetch::new(req)
        .upstream_path(UpstreamPath::Decoded(path.clone()))
        .cache_path(path.as_str())
        .accept(HeaderValue::from_static(MEDIA_TYPE))
        .content_type(MEDIA_TYPE);
    let cached = cx.proxy().fetch_document(fetch).await?;

    let base = req.head.repository_url(&cx.origin_repository().name);
    let package = name.clone();
    let max_versions = cfg.max_versions_per_package;
    let body = cached.body.clone();
    let parsed = pool
        .run(move || {
            let url = |version: &PubVersion| format!("{base}/{}", archive_path(&package, version));
            PackageDoc::parse(&body, &package, &ArchiveUrls::Rewrite(&url), max_versions)
        })
        .await?
        .map_err(|error| {
            // The text names the rule, never the content. The upstream address stays in the
            // log: format-core logs the error once, at the boundary.
            FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                format!("the upstream listing is unusable: {error}"),
            )
        })?;
    if parsed.dropped > 0 {
        metrics::listing_entries_dropped(parsed.dropped);
    }
    if parsed.doc.versions().is_empty() {
        return Err(FormatError::NotFound);
    }
    Ok((parsed.doc, cached.cache))
}
