//! Manifests through a proxy repository.
//!
//! A manifest by digest is immutable: a cached copy is served at once and for ever, and a
//! fetched one is verified against the digest of the request before anything is cached
//! (format-core writes, verifies and only then publishes and serves, because an expected
//! checksum is set). A manifest by tag is cached for the metadata TTL and revalidated; the
//! upstream's `Docker-Content-Digest`, when it sends one, is the expected checksum, so a body
//! that does not match its own announced digest is never cached or served.
//!
//! `Accept` is forwarded: the client's list is narrowed to the manifest types this registry
//! knows and sent in a fixed order. A tag can resolve to different documents for different
//! lists (an index or a single platform manifest), so a list that is not the full one gets
//! its own cache entry, `<tag>@accept-<hash>`, which can never collide with a real tag (a tag
//! has no `@`) and never shows up in tag listings.

use std::sync::Arc;

use axum_core::body::Body;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_format_core::{
    CachedDocument, ChecksumSource, ComponentSpec, ExpectedChecksum, FormatError, HashAlgo,
    ProxyFetch, UpstreamFailureKind,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::auth::UpstreamAuth;
use super::names::{upstream_name, upstream_path};
use super::{passthrough, pipeline_can_carry, token_was_refused};
use crate::error::OciError;
use crate::headers::{DOCKER_CONTENT_DIGEST, set_text, stamp_version};
use crate::manifests;
use crate::ops::Op;
use crate::parse::digest::{Digest, DigestAlgorithm};
use crate::parse::media_type::{Accept, MANIFEST_TYPES, ManifestType, essence};
use crate::parse::name::RepositoryName;
use crate::parse::reference::Reference;
use crate::store;

/// `GET` and `HEAD` of a manifest.
pub(crate) async fn read(
    op: &Op<'_>,
    name: &RepositoryName,
    reference: &Reference,
) -> Result<Response<Body>, OciError> {
    if reference.digest().is_some() {
        // Content addressed and verified when it was cached: no TTL applies.
        if let Some(asset) = store::get_asset(op.cx, op.req.path.as_str()).await? {
            manifests::negotiate(op, &asset.content_type)?;
            return manifests::serve(op, &asset).await;
        }
    }
    let upstream = upstream_name(op.cx, name)?;
    let accept = Accept::parse(op.header_values(&header::ACCEPT));
    let types = accept.manifest_types();
    if types.is_empty() {
        return Err(OciError::manifest_unknown());
    }
    let accept_header = HeaderValue::from_str(&types.join(", "))
        .map_err(|_| OciError::internal("an accept list of known media types is not a header"))?;
    let cache_path = cache_path(name, reference, &types);
    let tail = format!("manifests/{reference}");
    let mut refresh = false;
    let document = loop {
        let auth = op
            .shared
            .tokens
            .authorization(op.cx, &upstream, refresh)
            .await?;
        let attempt = fetch_once(
            op,
            name,
            reference,
            &upstream,
            &tail,
            &cache_path,
            &accept_header,
            &auth,
        )
        .await;
        match attempt {
            Err(error) if !refresh && token_was_refused(op, &upstream, &auth, &error).await => {
                refresh = true;
            }
            other => break other,
        }
    }
    .map_err(|error| {
        OciError::from(error).not_found_as(
            crate::error::OciErrorCode::ManifestUnknown,
            "manifest unknown to registry",
        )
    })?;
    respond(op, reference, &cache_path, document).await
}

/// One attempt to get the document: through the pipeline, or past it when the pipeline cannot
/// carry the credentials.
#[allow(
    clippy::too_many_arguments,
    reason = "one parameter per fact of the fetch; a struct would only rename them"
)]
async fn fetch_once(
    op: &Op<'_>,
    name: &RepositoryName,
    reference: &Reference,
    upstream: &str,
    tail: &str,
    cache_path: &str,
    accept: &HeaderValue,
    auth: &UpstreamAuth,
) -> Result<CachedDocument, FormatError> {
    if pipeline_can_carry(op, auth) {
        let fetch = pipeline_fetch(
            op, name, reference, upstream, tail, cache_path, accept, auth,
        )?;
        op.cx.proxy().fetch_document(fetch).await
    } else if let UpstreamAuth::Bearer(bearer) = auth {
        passthrough::manifest(op, upstream, tail, reference, accept, bearer).await
    } else {
        Err(FormatError::internal(
            "no way to carry the upstream request",
        ))
    }
}

/// The cache key of a manifest: the request path, plus a hash of the accept list when it is
/// not the full list and the reference is a tag.
fn cache_path(name: &RepositoryName, reference: &Reference, types: &[&str]) -> String {
    let base = store::manifest_path(name, &reference.to_string());
    if reference.digest().is_some() || types.len() == MANIFEST_TYPES.len() {
        return base;
    }
    let hash = hex::encode(Sha256::digest(types.join(",").as_bytes()));
    format!("{base}@accept-{}", hash.get(..8).unwrap_or(&hash))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one parameter per fact of the fetch; a struct would only rename them"
)]
fn pipeline_fetch<'a>(
    op: &'a Op<'_>,
    name: &RepositoryName,
    reference: &Reference,
    upstream: &str,
    tail: &str,
    cache_path: &str,
    accept: &HeaderValue,
    auth: &UpstreamAuth,
) -> Result<ProxyFetch<'a>, FormatError> {
    let mut fetch = ProxyFetch::new(op.req)
        .upstream_path(upstream_path(upstream, tail)?)
        .cache_path(cache_path)
        .accept(accept.clone())
        .component(ComponentSpec {
            group: None,
            name: name.as_str().to_owned(),
            version: Some(reference.to_string()),
        });
    match reference {
        Reference::Digest(digest) if digest.algorithm() == DigestAlgorithm::Sha256 => {
            let expected = ExpectedChecksum::new(HashAlgo::Sha256, digest.hex())
                .map_err(|_| FormatError::internal("a validated digest is not a valid checksum"))?;
            fetch = fetch.expect(expected);
        }
        Reference::Digest(_) => {}
        Reference::Tag(_) => fetch = fetch.checksum_source(Arc::new(DockerContentDigest)),
    }
    if let UpstreamAuth::Bearer(bearer) = auth {
        fetch = fetch.header(header::AUTHORIZATION, bearer.clone());
    }
    Ok(fetch)
}

/// Turns the document into the response: the media type, the digest and the validators.
async fn respond(
    op: &Op<'_>,
    reference: &Reference,
    cache_path: &str,
    document: CachedDocument,
) -> Result<Response<Body>, OciError> {
    let cap = op.config().manifest_max_bytes;
    if document.body.len() as u64 > cap {
        return Err(FormatError::upstream(
            UpstreamFailureKind::BadStatus,
            "the upstream manifest is larger than the limit",
        )
        .into());
    }
    if let Reference::Digest(digest) = reference
        && digest.algorithm() != DigestAlgorithm::Sha256
        && !digest.verifies(&document.body)
    {
        // The pipeline can only verify SHA-256, so this copy was cached unverified: remove it.
        let removed = op
            .cx
            .ports()
            .assets
            .delete_by_path(op.cx.repository().id, cache_path)
            .await;
        if let Err(error) = removed {
            tracing::warn!(%error, "could not remove a manifest copy that failed its digest check");
        }
        return Err(FormatError::upstream(
            UpstreamFailureKind::Integrity,
            "the upstream manifest does not match its digest",
        )
        .into());
    }
    let media_type =
        detect_media_type(&document.body, &document.content_type).ok_or_else(|| {
            OciError::from(FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                "the upstream answered with a document that is not a supported manifest",
            ))
        })?;
    manifests::negotiate(op, media_type)?;
    let digest = Digest::of(DigestAlgorithm::Sha256, &document.body);
    let length = document.body.len() as u64;
    let head = op.req.head.method == Method::HEAD;
    let mut headers: HeaderMap = document.response_headers();
    let mut response = Response::new(if head {
        Body::empty()
    } else {
        Body::from(document.body)
    });
    *response.status_mut() = StatusCode::OK;
    set_text(&mut headers, header::CONTENT_TYPE, media_type);
    set_text(&mut headers, DOCKER_CONTENT_DIGEST, &digest.to_string());
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    stamp_version(&mut headers);
    response.headers_mut().extend(headers);
    Ok(response)
}

/// The manifest type of an upstream document: its own `mediaType`, else the `Content-Type`.
/// Servers disagree about the header (`application/json` is common), the document wins.
pub(super) fn detect_media_type(body: &[u8], content_type: &str) -> Option<&'static str> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Peek {
        media_type: Option<String>,
    }
    let from_body = serde_json::from_slice::<Peek>(body)
        .ok()
        .and_then(|peek| peek.media_type)
        .and_then(|media_type| ManifestType::of(&media_type));
    let kind = from_body.or_else(|| essence(content_type).and_then(|e| ManifestType::of(&e)))?;
    Some(kind.media_type())
}

/// Reads `Docker-Content-Digest` of an upstream response as the checksum the body must have.
struct DockerContentDigest;

impl ChecksumSource for DockerContentDigest {
    fn expected_from_headers(&self, headers: &HeaderMap) -> Option<ExpectedChecksum> {
        let value = headers.get("docker-content-digest")?.to_str().ok()?;
        let digest = Digest::parse(value).ok()?;
        if digest.algorithm() == DigestAlgorithm::Sha256 {
            ExpectedChecksum::new(HashAlgo::Sha256, digest.hex()).ok()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests;
