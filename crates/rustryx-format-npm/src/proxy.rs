//! Proxy repositories: an npm registry mirrored through this one.
//!
//! * **Package documents** are fetched from the upstream with the format core's proxy pipeline
//!   (singleflight, cache by `metadata_max_age_secs`, revalidation with the upstream's validators,
//!   negative cache, serve-stale), then parsed on the blocking pool and rewritten so that every
//!   `dist.tarball` points at the repository the client addressed. The two variants are cached
//!   separately: the full document under `<package>`, the abbreviated one under
//!   `<package>/-/abbreviated.json`.
//! * **Tarballs** are only fetched for versions the upstream's document lists, and are verified
//!   before the first byte reaches a client: the digest of the version in the (cached) upstream
//!   document is the expected checksum of the pipeline, so a body that does not match is neither
//!   cached nor served. The blob commit knows SHA-1, SHA-256 and MD5 only, so the expectation is
//!   the SHA-256 of `dist.integrity` when it has one, else the SHA-1 (`dist.shasum`); the SHA-512
//!   `integrity` of the document is passed through unchanged and the npm client verifies it
//!   itself. A version without any usable digest is refused.
//! * Upstream credentials are the pipeline's: they are never forwarded to a client and are
//!   dropped when an upstream redirects to another origin. The `dist.tarball` URLs of the upstream
//!   are never followed: the tarball is requested at `<name>/-/<file>` below the configured
//!   `remote_url`, so a document cannot steer the proxy to another host.
//! * The upstream's `401` and `403` are `502`s (format core), never forwarded.

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Response};
use rustryx_format_core::conditional::if_none_match_hits;
use rustryx_format_core::proxy::CachedDocument;
use rustryx_format_core::{
    ArtifactRequest, ComponentSpec, ExpectedChecksum, FormatContext, FormatError, HashAlgo,
    ProxyFetch, UpstreamFailureKind, UpstreamPath,
};
use semver::Version;
use serde_json::Value;

use crate::handler::NpmHandler;
use crate::metrics;
use crate::name::PackageName;
use crate::packument::{Packument, ParseMode};
use crate::publish::parse_integrity;
use crate::respond;
use crate::route::Route;
use crate::search::empty_search;
use crate::variant::Variant;
use crate::view::{Part, RenderSource, document_response, not_modified, view_etag};

/// Cache path of the abbreviated variant of a package's document.
fn abbreviated_cache_path(package: &PackageName) -> String {
    format!("{package}/-/abbreviated.json")
}

/// What the upstream document says about a tarball.
enum Lookup {
    /// The version is not in the document.
    NotListed,
    /// The version is listed but carries no digest this registry can verify.
    NoDigest,
    /// The version is listed with a digest to verify against.
    Digest(ExpectedChecksum),
}

fn digest_of_version(manifest: &serde_json::Map<String, Value>) -> Option<ExpectedChecksum> {
    let dist = manifest.get("dist")?.as_object()?;
    let integrity = dist
        .get("integrity")
        .and_then(Value::as_str)
        .and_then(parse_integrity);
    if let Some(sha256) = integrity.as_ref().and_then(|i| i.sha256)
        && let Ok(expected) = ExpectedChecksum::new(HashAlgo::Sha256, &hex::encode(sha256))
    {
        return Some(expected);
    }
    let sha1 = dist
        .get("shasum")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| integrity.and_then(|i| i.sha1).map(hex::encode))?;
    ExpectedChecksum::new(HashAlgo::Sha1, &sha1).ok()
}

fn lookup(bytes: &[u8], package: &PackageName, version: &Version) -> Result<Lookup, FormatError> {
    let document = Packument::from_slice(bytes, ParseMode::Lenient, Some(package))
        .map_err(|e| crate::view::upstream_error(&e))?;
    Ok(match document.versions().get(version) {
        None => Lookup::NotListed,
        Some(manifest) => digest_of_version(manifest).map_or(Lookup::NoDigest, Lookup::Digest),
    })
}

impl NpmHandler {
    /// Serves a request on a proxy repository.
    pub(crate) async fn proxied(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        route: Route,
    ) -> Result<Response<Body>, FormatError> {
        match route {
            Route::Packument(package) => self.proxy_packument(cx, req, &package).await,
            Route::VersionDoc { package, spec } => {
                self.proxy_part(cx, req, &package, Variant::Full, Part::Version(spec))
                    .await
            }
            Route::DistTags(package) => {
                self.proxy_part(cx, req, &package, Variant::Abbreviated, Part::DistTags)
                    .await
            }
            Route::Tarball { package, version } => {
                self.proxy_tarball(cx, req, &package, &version).await
            }
            Route::Search => Ok(empty_search(req.method())),
            // Writes are refused by the engine before they get here, and the session routes are
            // answered before the repository type is looked at.
            _ => Err(FormatError::NotFound),
        }
    }

    async fn upstream_document(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        package: &PackageName,
        variant: Variant,
    ) -> Result<CachedDocument, FormatError> {
        let cache_path = match variant {
            Variant::Full => package.full(),
            Variant::Abbreviated => abbreviated_cache_path(package),
        };
        let fetch = ProxyFetch::new(req)
            // npm sends `@scope%2fname`, and every registry accepts that form.
            .upstream_path(UpstreamPath::Encoded(package.escaped()))
            .cache_path(cache_path)
            .accept(HeaderValue::from_static(variant.media_type()))
            .content_type("application/json");
        let document = cx.proxy().fetch_document(fetch).await?;
        let limit = self.config.max_packument_bytes;
        if document.body.len() as u64 > limit {
            return Err(FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                format!(
                    "the upstream package document of {} bytes exceeds the limit of {limit}",
                    document.body.len()
                ),
            ));
        }
        Ok(document)
    }

    async fn proxy_packument(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        package: &PackageName,
    ) -> Result<Response<Body>, FormatError> {
        let variant = Variant::of_headers(&req.head.headers);
        let document = self.upstream_document(cx, req, package, variant).await?;
        let base = req.head.repository_url(&cx.origin_repository().name);
        let etag = view_etag(&document.etag, variant, &base);
        let extra = document.response_headers();
        if if_none_match_hits(&req.head.headers, &etag) {
            return Ok(not_modified(&etag, extra));
        }
        let body = self
            .render_document(
                cx.limits().blocking_permit_wait,
                RenderSource {
                    bytes: document.body.clone(),
                    mode: ParseMode::Lenient,
                    package: package.clone(),
                    variant,
                    base,
                    digest: document.etag.trim_matches('"').to_owned(),
                },
            )
            .await?;
        metrics::packument_served("proxy", variant.label());
        Ok(document_response(req.method(), variant, &etag, body, extra))
    }

    async fn proxy_part(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        package: &PackageName,
        variant: Variant,
        part: Part,
    ) -> Result<Response<Body>, FormatError> {
        let document = self.upstream_document(cx, req, package, variant).await?;
        let rendered: Bytes = self
            .render_part(
                cx.limits().blocking_permit_wait,
                RenderSource {
                    bytes: document.body.clone(),
                    mode: ParseMode::Lenient,
                    package: package.clone(),
                    variant,
                    base: req.head.repository_url(&cx.origin_repository().name),
                    digest: document.etag.trim_matches('"').to_owned(),
                },
                part,
            )
            .await?
            .ok_or(FormatError::NotFound)?;
        let mut response =
            respond::bytes_with_type(http::StatusCode::OK, "application/json", rendered);
        respond::extend_headers(&mut response, cache_headers(&document));
        Ok(respond::for_method(req.method(), response))
    }

    async fn proxy_tarball(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        package: &PackageName,
        version: &Version,
    ) -> Result<Response<Body>, FormatError> {
        let document = self
            .upstream_document(cx, req, package, Variant::Abbreviated)
            .await?;
        let (bytes, owned, wanted) = (document.body.clone(), package.clone(), version.clone());
        let found = self
            .blocking
            .run(cx.limits().blocking_permit_wait, move || {
                lookup(&bytes, &owned, &wanted)
            })
            .await??;
        let expected = match found {
            Lookup::Digest(expected) => expected,
            Lookup::NotListed => {
                metrics::proxy_tarball_refused("not_listed");
                return Err(FormatError::NotFound);
            }
            Lookup::NoDigest => {
                metrics::proxy_tarball_refused("no_digest");
                return Err(FormatError::upstream(
                    UpstreamFailureKind::Integrity,
                    "the upstream lists the version without a digest this registry can verify",
                ));
            }
        };
        let canonical = package.canonical();
        let fetch = ProxyFetch::new(req)
            .expect(expected)
            .content_type("application/octet-stream")
            .component(ComponentSpec {
                group: canonical.scope().map(str::to_owned),
                name: canonical.name().to_owned(),
                version: Some(version.to_string()),
            });
        cx.proxy().fetch_artifact(fetch).await
    }
}

/// The cache markers of a proxied document, without its validator (the rendered answer has its
/// own).
fn cache_headers(document: &CachedDocument) -> HeaderMap {
    let mut headers = document.response_headers();
    headers.remove(http::header::ETAG);
    headers
}
