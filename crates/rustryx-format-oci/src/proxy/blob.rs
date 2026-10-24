//! Blobs through a proxy repository.
//!
//! A blob is its digest: a copy that is cached is served for ever (no TTL, so a layer does not
//! expire and get fetched again), and a copy that is fetched is written to the blob store,
//! hashed while it is written, compared with the digest of the request and only then published
//! and served (the expected checksum makes the pipeline skip the tee). The upstream's redirect
//! to a CDN is followed by the executor, which strips the registry token on the way to another
//! origin and re-checks every hop against the SSRF policy.

use axum_core::body::Body;
use http::{HeaderValue, Response, header};
use rustryx_format_core::{ExpectedChecksum, FormatError, HashAlgo, ProxyFetch};

use super::auth::UpstreamAuth;
use super::names::{upstream_name, upstream_path};
use super::{passthrough, pipeline_can_carry, token_was_refused};
use crate::error::{OciError, OciErrorCode};
use crate::headers::{DOCKER_CONTENT_DIGEST, set_text, stamp_version};
use crate::ops::Op;
use crate::parse::digest::{Digest, DigestAlgorithm};
use crate::parse::media_type::OCTET_STREAM;
use crate::parse::name::RepositoryName;

/// `GET` and `HEAD` of a blob.
pub(crate) async fn read(
    op: &Op<'_>,
    name: &RepositoryName,
    digest: &Digest,
) -> Result<Response<Body>, OciError> {
    match op.cx.hosted().download(op.req).await {
        Ok(mut response) => {
            stamp(&mut response, digest);
            return Ok(response);
        }
        Err(FormatError::NotFound) => {}
        Err(error) => return Err(error.into()),
    }
    if digest.algorithm() != DigestAlgorithm::Sha256 {
        // The pipeline verifies SHA-256 only: nothing else may be cached or served unchecked.
        return Err(OciError::blob_unknown());
    }
    let upstream = upstream_name(op.cx, name)?;
    let tail = format!("blobs/{digest}");
    let mut refresh = false;
    let mut response = loop {
        let auth = op
            .shared
            .tokens
            .authorization(op.cx, &upstream, refresh)
            .await?;
        let attempt = fetch_once(op, &upstream, &tail, digest, &auth).await;
        match attempt {
            Err(error) if !refresh && token_was_refused(op, &upstream, &auth, &error).await => {
                refresh = true;
            }
            other => break other,
        }
    }
    .map_err(|error| {
        OciError::from(error).not_found_as(OciErrorCode::BlobUnknown, "blob unknown to registry")
    })?;
    stamp(&mut response, digest);
    Ok(response)
}

/// One attempt to get the blob: through the pipeline, or past it when it cannot carry the
/// credentials.
async fn fetch_once(
    op: &Op<'_>,
    upstream: &str,
    tail: &str,
    digest: &Digest,
    auth: &UpstreamAuth,
) -> Result<Response<Body>, FormatError> {
    if pipeline_can_carry(op, auth) {
        let expected = ExpectedChecksum::new(HashAlgo::Sha256, digest.hex())
            .map_err(|_| FormatError::internal("a validated digest is not a valid checksum"))?;
        let mut fetch = ProxyFetch::new(op.req)
            .upstream_path(upstream_path(upstream, tail)?)
            .expect(expected)
            .content_type(OCTET_STREAM);
        if let UpstreamAuth::Bearer(bearer) = auth {
            fetch = fetch.header(header::AUTHORIZATION, bearer.clone());
        }
        op.cx.proxy().fetch_artifact(fetch).await
    } else if let UpstreamAuth::Bearer(bearer) = auth {
        passthrough::blob(op, upstream, tail, digest, bearer).await
    } else {
        Err(FormatError::internal(
            "no way to carry the upstream request",
        ))
    }
}

fn stamp(response: &mut Response<Body>, digest: &Digest) {
    let headers = response.headers_mut();
    set_text(headers, DOCKER_CONTENT_DIGEST, &digest.to_string());
    stamp_version(headers);
    if !headers.contains_key(header::CONTENT_TYPE) {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(OCTET_STREAM));
    }
}
