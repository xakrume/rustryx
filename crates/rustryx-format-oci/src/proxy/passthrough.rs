//! The one case the proxy pipeline cannot carry: a bearer token AND stored Basic credentials.
//!
//! The pipeline puts the repository's Basic credentials on every call it makes, over any
//! `Authorization` header the format sets, so a registry that takes its token only (Docker
//! Hub with an account, GHCR, GitLab) would be sent Basic where it wants Bearer and refuse
//! every data request. The token handshake itself works (it needs Basic, towards the token
//! service), so this module makes the data request itself through the same executor, which
//! still applies the SSRF policy, breaker, concurrency cap, timeouts, retries and redirect
//! rules, with the token and no Basic.
//!
//! What it gives up, and why that is acceptable until format-core can say "no stored
//! credentials" (`docs/format-oci.md`, "format-core gaps"):
//!
//! * nothing is cached: every pull of such a repository goes to the upstream (`X-Rustryx-Cache`
//!   says `MISS`);
//! * a manifest is verified before it is returned (digest requests), but a blob streams: its
//!   digest is checked while it passes and, at its end, a mismatch breaks the stream, so the
//!   client sees a failed transfer and its own digest check fails as well;
//! * ranges are not forwarded.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum_core::body::Body;
use bytes::Bytes;
use futures::Stream;
use futures::stream::BoxStream;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::error::UpstreamError as CoreUpstreamError;
use rustryx_format_core::proxy::executor::{CallClass, UpstreamCall, UpstreamResult};
use rustryx_format_core::proxy::upstream::UpstreamError;
use rustryx_format_core::stream::{HASH_SLICE, collect_bounded};
use rustryx_format_core::{CacheStatus, CachedDocument, FormatError, UpstreamFailureKind};
use sha2::{Digest as _, Sha256};

use super::names::upstream_url;
use crate::ops::Op;
use crate::parse::digest::Digest;
use crate::parse::media_type::OCTET_STREAM;
use crate::parse::reference::Reference;

/// A manifest fetched without the cache.
pub(super) async fn manifest(
    op: &Op<'_>,
    upstream: &str,
    tail: &str,
    reference: &Reference,
    accept: &HeaderValue,
    bearer: &HeaderValue,
) -> Result<CachedDocument, FormatError> {
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, accept.clone());
    let result = call(
        op,
        upstream,
        tail,
        Method::GET,
        headers,
        bearer,
        CallClass::Metadata,
    )
    .await?;
    let content_type = result
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = collect_bounded(result.body, op.config().manifest_max_bytes)
        .await
        .map_err(|_| {
            FormatError::upstream(
                UpstreamFailureKind::BadStatus,
                "the upstream manifest could not be read within its limit",
            )
        })?;
    if let Reference::Digest(digest) = reference
        && !digest.verifies(&body)
    {
        return Err(FormatError::upstream(
            UpstreamFailureKind::Integrity,
            "the upstream manifest does not match its digest",
        ));
    }
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(&body)));
    Ok(CachedDocument {
        body,
        content_type,
        cache: CacheStatus::Miss,
        last_modified: op.cx.now(),
        etag,
    })
}

/// A blob streamed from the upstream while its digest is checked.
pub(super) async fn blob(
    op: &Op<'_>,
    upstream: &str,
    tail: &str,
    digest: &Digest,
    bearer: &HeaderValue,
) -> Result<Response<Body>, FormatError> {
    let head = op.req.head.method == Method::HEAD;
    let method = if head { Method::HEAD } else { Method::GET };
    let result = call(
        op,
        upstream,
        tail,
        method,
        HeaderMap::new(),
        bearer,
        CallClass::Data,
    )
    .await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(OCTET_STREAM));
    if let Some(length) = result.headers.get(header::CONTENT_LENGTH) {
        headers.insert(header::CONTENT_LENGTH, length.clone());
    }
    headers.insert(
        rustryx_format_core::hosted::X_RUSTRYX_CACHE,
        HeaderValue::from_static(CacheStatus::Miss.as_str()),
    );
    if !head {
        *response.body_mut() = Body::from_stream(Verifying::new(result.body, digest));
    }
    Ok(response)
}

/// One data or document request with the bearer token and no stored credentials.
async fn call(
    op: &Op<'_>,
    upstream: &str,
    tail: &str,
    method: Method,
    mut headers: HeaderMap,
    bearer: &HeaderValue,
    class: CallClass,
) -> Result<UpstreamResult, FormatError> {
    let repository = op.cx.repository();
    let config = repository
        .proxy
        .as_ref()
        .ok_or_else(|| FormatError::internal("proxy repository without proxy settings"))?;
    let mut bearer = bearer.clone();
    bearer.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, bearer);
    let result = op
        .cx
        .upstream()
        .execute(UpstreamCall {
            repository: repository.id,
            repo_name: repository.name.to_string(),
            method,
            url: upstream_url(op.cx, upstream, tail, &[])?,
            headers,
            credentials: None,
            max_concurrent: config.max_concurrent_upstream,
            class,
        })
        .await
        .map_err(|error| FormatError::upstream(error.kind(), error.to_string()))?;
    match result.status.as_u16() {
        200 => Ok(result),
        404 | 410 => Err(FormatError::NotFound),
        // Never passed on: the client cannot fix the credentials of this proxy.
        401 | 403 => Err(FormatError::upstream(
            UpstreamFailureKind::Rejected,
            "the upstream refused the registry token",
        )),
        other => Err(FormatError::from(CoreUpstreamError::Status(other))),
    }
}

/// A body stream that hashes what passes and fails at its end when the digest is wrong.
struct Verifying {
    inner: BoxStream<'static, Result<Bytes, UpstreamError>>,
    hasher: Option<Sha256>,
    expected: String,
}

impl Verifying {
    fn new(inner: BoxStream<'static, Result<Bytes, UpstreamError>>, digest: &Digest) -> Self {
        Self {
            inner,
            hasher: Some(Sha256::new()),
            expected: digest.hex().to_owned(),
        }
    }
}

impl Stream for Verifying {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => {
                if let Some(hasher) = self.hasher.as_mut() {
                    // Slices of at most 64 KiB, in this task, as the hashing rule says.
                    for slice in chunk.chunks(HASH_SLICE) {
                        hasher.update(slice);
                    }
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(error))) => {
                Poll::Ready(Some(Err(io::Error::other(error.to_string()))))
            }
            Poll::Ready(None) => match self.hasher.take() {
                None => Poll::Ready(None),
                Some(hasher) => {
                    if hex::encode(hasher.finalize()) == self.expected {
                        Poll::Ready(None)
                    } else {
                        Poll::Ready(Some(Err(io::Error::other(
                            "the upstream blob does not match its digest",
                        ))))
                    }
                }
            },
        }
    }
}
