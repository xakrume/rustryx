//! Proxy repositories: metadata revalidated by TTL, immutable files cached for good, snapshot
//! pointers resolved through the version metadata, files verified against their `.sha1`.

use std::sync::Arc;

use axum_core::body::Body;
use http::{HeaderMap, HeaderValue, Method, Response, header};
use rustryx_format_core::hosted::X_RUSTRYX_CACHE;
use rustryx_format_core::{
    ArtifactRequest, CachedDocument, ComponentSpec, ExpectedChecksum, FormatContext, FormatError,
    HashAlgo, ProxyFetch, StandardChecksumHeaders,
};

use crate::checksum::{digest_of_bytes, parse_checksum_file};
use crate::config::VersionPolicy;
use crate::handler::MavenHandler;
use crate::layout::{ArtifactPath, ChecksumAlgo, MavenPath, MetadataLevel, parse_snapshot_value};
use crate::metadata::parse_metadata;
use crate::metrics;
use crate::request::with_path;
use crate::response::{Document, checksum_document, respond};

/// The sidecar digests tried, in this order. `.sha1` is what every Maven repository has and `.sha256`
/// what newer ones add; each probe that misses costs one upstream request (remembered by the
/// negative cache), so the list stays short. The pipeline has no SHA-512 expectation.
const SIDECARS: [(ChecksumAlgo, HashAlgo); 2] = [
    (ChecksumAlgo::Sha1, HashAlgo::Sha1),
    (ChecksumAlgo::Sha256, HashAlgo::Sha256),
];

impl MavenHandler {
    /// `GET` and `HEAD` on a proxy repository.
    pub(crate) async fn proxy_get(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        parsed: Option<MavenPath>,
    ) -> Result<Response<Body>, FormatError> {
        // Outside the layout (`archetype-catalog.xml`, `.meta/prefixes.txt`): forwarded and cached
        // like any file, for the content lifetime.
        let Some(parsed) = parsed else {
            return cx.serve_default(req.clone(), Body::empty()).await;
        };
        if let Some(version) = parsed.version() {
            let policy = VersionPolicy::of(cx.repository())?;
            if !policy.allows(version) {
                // The upstream is not asked for a kind of version this repository does not take.
                metrics::rejection("version_policy");
                return Err(FormatError::NotFound);
            }
        }
        match parsed {
            MavenPath::Metadata { level, checksum } => {
                proxy_metadata(cx, req, &level, checksum).await
            }
            MavenPath::Artifact { file, checksum } => {
                self.proxy_artifact(cx, req, file, checksum).await
            }
        }
    }

    async fn proxy_artifact(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        file: ArtifactPath,
        checksum: Option<ChecksumAlgo>,
    ) -> Result<Response<Body>, FormatError> {
        if file.is_snapshot() && file.stamp.is_none() {
            // `artifact-1.0-SNAPSHOT.jar` names whatever the newest build is: it can change at the
            // upstream any time, so it is resolved through the version metadata, which is
            // revalidated by the metadata TTL, and the build it names is immutable.
            return match self.resolve_remote_pointer(cx, req, &file).await? {
                Some(stamped) => {
                    let mut path = stamped.to_path_string();
                    if let Some(algo) = checksum {
                        path = format!("{path}.{}", algo.suffix());
                    }
                    let resolved = with_path(req, &path)?;
                    immutable(cx, &resolved, &stamped, checksum).await
                }
                // A non unique snapshot repository has no version metadata: the file itself is
                // fetched and lives for the content lifetime of the repository.
                None => cx.serve_default(req.clone(), Body::empty()).await,
            };
        }
        immutable(cx, req, &file, checksum).await
    }

    async fn resolve_remote_pointer(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        file: &ArtifactPath,
    ) -> Result<Option<ArtifactPath>, FormatError> {
        let level = MetadataLevel::Version {
            group_id: file.group_id.clone(),
            artifact_id: file.artifact_id.clone(),
            version: file.version.clone(),
        };
        let document_request = with_path(req, &level.document_path())?;
        let document = match cx
            .proxy()
            .fetch_document(ProxyFetch::new(&document_request))
            .await
        {
            Ok(document) => document,
            Err(FormatError::NotFound) => {
                metrics::metadata_op("snapshot_resolve", "not_found");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let bytes = document.body;
        let Ok(metadata) = self.pool.run(move || parse_metadata(&bytes)).await? else {
            metrics::metadata_op("snapshot_resolve", "unparsable");
            return Ok(None);
        };
        let stamp = metadata
            .versioning
            .iter()
            .flat_map(|versioning| versioning.snapshot_versions.iter())
            .find(|entry| entry.classifier == file.classifier && entry.extension == file.extension)
            .and_then(|entry| parse_snapshot_value(&file.version, &entry.value));
        metrics::metadata_op(
            "snapshot_resolve",
            if stamp.is_some() { "ok" } else { "not_found" },
        );
        Ok(stamp.map(|stamp| file.with_stamp(stamp)))
    }
}

/// The cache marker and the stale warning of a cached document. The validators are not copied:
/// for a checksum derived from the document they would describe the wrong thing.
fn cache_headers(document: &CachedDocument) -> HeaderMap {
    let all = document.response_headers();
    let mut headers = HeaderMap::new();
    for name in [X_RUSTRYX_CACHE, header::WARNING] {
        if let Some(value) = all.get(&name) {
            headers.insert(name, value.clone());
        }
    }
    headers
}

/// `maven-metadata.xml` (revalidated by the metadata TTL, served byte for byte so that any
/// signature or digest the upstream published for it stays valid) and its checksum files (derived
/// from the cached document, so they always describe the bytes served: fetching the upstream's own
/// `.sha1` separately races with a metadata change in between).
async fn proxy_metadata(
    cx: &FormatContext,
    req: &ArtifactRequest,
    level: &MetadataLevel,
    checksum: Option<ChecksumAlgo>,
) -> Result<Response<Body>, FormatError> {
    let document_request = match checksum {
        Some(_) => with_path(req, &level.document_path())?,
        None => req.clone(),
    };
    let fetch =
        ProxyFetch::new(&document_request).checksum_source(Arc::new(StandardChecksumHeaders));
    let document = cx.proxy().fetch_document(fetch).await?;
    let extra = cache_headers(&document);
    match checksum {
        None => respond(
            &req.head,
            Document::new(document.body.clone(), "application/xml")
                .modified(Some(document.last_modified))
                .etag(document.etag.clone())
                .extra(extra),
        ),
        Some(algo) => respond(
            &req.head,
            checksum_document(
                digest_of_bytes(algo, &document.body),
                Some(document.last_modified),
            )
            .extra(extra),
        ),
    }
}

/// A file that never changes at the upstream: a release, a timestamped snapshot build, and the
/// checksum or signature files of either.
///
/// Once cached it is served without asking the upstream again, whatever its age
/// (`content_max_age_secs` is not consulted): re-releasing a version is not something the
/// Maven Central policy allows and a mirror must not turn it into a silent change.
async fn immutable(
    cx: &FormatContext,
    req: &ArtifactRequest,
    file: &ArtifactPath,
    checksum: Option<ChecksumAlgo>,
) -> Result<Response<Body>, FormatError> {
    let cached = cx
        .ports()
        .assets
        .get_by_path(cx.repository().id, req.path.as_str())
        .await?
        .is_some();
    if cached {
        let mut response = cx.hosted().download(req).await?;
        response
            .headers_mut()
            .insert(X_RUSTRYX_CACHE, HeaderValue::from_static("HIT"));
        return Ok(response);
    }
    let mut fetch = ProxyFetch::new(req);
    if checksum.is_none() {
        fetch = fetch.component(ComponentSpec {
            group: Some(file.group_id.clone()),
            name: file.artifact_id.clone(),
            version: Some(file.version.clone()),
        });
    }
    // A `HEAD` never fills the cache, and a sidecar is itself the thing that verifies.
    if checksum.is_some() || req.head.method == Method::HEAD {
        return cx
            .proxy()
            .fetch_artifact(fetch.checksum_source(Arc::new(StandardChecksumHeaders)))
            .await;
    }
    let fetch = match sidecar_checksum(cx, req).await? {
        // Verified before the first byte is served, and a mismatch is neither served nor cached.
        Some(expected) => fetch.expect(expected),
        // No sidecar: a digest the upstream announces in its headers still counts.
        None => fetch.checksum_source(Arc::new(StandardChecksumHeaders)),
    };
    cx.proxy().fetch_artifact(fetch).await
}

/// The digest the upstream publishes next to the file, `None` when it has no usable one.
///
/// A missing or unreadable sidecar leaves the file unverified (a repository without sidecars is
/// legitimate). A sidecar that is there and wrong is not ignored: it is the expectation the
/// pipeline checks the file against. The probe treats a 403 as a miss, because object stores
/// answer 403 for a missing key; a 401 is still a failure.
async fn sidecar_checksum(
    cx: &FormatContext,
    req: &ArtifactRequest,
) -> Result<Option<ExpectedChecksum>, FormatError> {
    for (algo, hash) in SIDECARS {
        let sidecar = with_path(req, &format!("{}.{}", req.path, algo.suffix()))?;
        let document = match cx
            .proxy()
            .fetch_document(ProxyFetch::new(&sidecar).forbidden_is_miss())
            .await
        {
            Ok(document) => document,
            Err(FormatError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        if let Ok(hex) = parse_checksum_file(&document.body, algo)
            && let Ok(expected) = ExpectedChecksum::new(hash, &hex)
        {
            return Ok(Some(expected));
        }
    }
    Ok(None)
}
