//! What a client is shown of a package document: rendering, validators and the response.
//!
//! Hosted and proxy documents are rendered the same way: parsed on the blocking pool, every
//! `dist.tarball` rewritten to the repository the client addressed, then serialized as the full or
//! the abbreviated document. The `ETag` is derived from what the answer depends on (the stored
//! bytes, the variant, the repository URL), so a client's `If-None-Match` is answered with `304`
//! without parsing anything.

use std::time::Duration;

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_format_core::FormatError;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::handler::NpmHandler;
use crate::name::PackageName;
use crate::packument::{Packument, PackumentError, ParseMode, rev_of};
use crate::respond;
use crate::variant::Variant;

/// The validator of a rendered document.
#[must_use]
pub fn view_etag(source_digest: &str, variant: Variant, base: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source_digest.trim_matches('"').as_bytes());
    hasher.update([0]);
    hasher.update(variant.label().as_bytes());
    hasher.update([0]);
    hasher.update(base.as_bytes());
    let digest = hasher.finalize();
    format!("\"{}\"", hex::encode(&digest[..16]))
}

/// The answer to a conditional request that hit: `304` with the validator and the extra headers.
#[must_use]
pub fn not_modified(etag: &str, mut extra: HeaderMap) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    if let Ok(value) = HeaderValue::from_str(etag) {
        extra.insert(header::ETAG, value);
    }
    response.headers_mut().extend(extra);
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept"));
    response
}

/// A `200` with the rendered document. `extra` carries headers such as the cache marker of a
/// proxied copy; the content type, length, validator and `Vary` are set here.
#[must_use]
pub fn document_response(
    method: &Method,
    variant: Variant,
    etag: &str,
    body: Bytes,
    extra: HeaderMap,
) -> Response<Body> {
    let mut response = respond::bytes_with_type(StatusCode::OK, variant.media_type(), body);
    let headers = response.headers_mut();
    headers.extend(extra);
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    // The document differs by `Accept`, and clients revalidate it on every use: it is cheap to
    // answer `304` and wrong to serve a stale version list (a publish must be visible at once).
    headers.insert(header::VARY, HeaderValue::from_static("Accept"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    respond::for_method(method, response)
}

/// Maps a parse failure of a document this registry wrote: always a fault on our side.
pub(crate) fn stored_error(error: &PackumentError) -> FormatError {
    FormatError::internal(format!("a stored package document is unreadable: {error}"))
}

/// Maps a parse failure of a document an upstream or a member wrote: a bad upstream answer.
pub(crate) fn upstream_error(error: &PackumentError) -> FormatError {
    FormatError::upstream(
        rustryx_format_core::UpstreamFailureKind::BadStatus,
        format!("the upstream package document is unusable: {error}"),
    )
}

impl NpmHandler {
    /// Parses `bytes`, points every tarball at `base` and serializes the requested variant.
    ///
    /// `digest` is the SHA-256 (hex) of the source bytes, from which `_rev` is derived.
    pub(crate) async fn render_document(
        &self,
        wait: Duration,
        source: RenderSource,
    ) -> Result<Bytes, FormatError> {
        let RenderSource {
            bytes,
            mode,
            package,
            variant,
            base,
            digest,
        } = source;
        self.blocking
            .run(wait, move || {
                let mut document = Packument::from_slice(&bytes, mode, Some(&package)).map_err(
                    |e| match mode {
                        ParseMode::Strict => stored_error(&e),
                        ParseMode::Lenient => upstream_error(&e),
                    },
                )?;
                document.rewrite_tarballs(&package, &base);
                let value = match variant {
                    Variant::Full => {
                        document.to_value(Some(&rev_of(document.versions().len(), &digest)))
                    }
                    Variant::Abbreviated => document.abbreviated(),
                };
                Ok::<_, FormatError>(Bytes::from(value.to_string()))
            })
            .await?
    }

    /// Parses a document and answers a version document or the dist-tags of it.
    pub(crate) async fn render_part(
        &self,
        wait: Duration,
        source: RenderSource,
        part: Part,
    ) -> Result<Option<Bytes>, FormatError> {
        let RenderSource {
            bytes,
            mode,
            package,
            base,
            ..
        } = source;
        self.blocking
            .run(wait, move || {
                let mut document = Packument::from_slice(&bytes, mode, Some(&package)).map_err(
                    |e| match mode {
                        ParseMode::Strict => stored_error(&e),
                        ParseMode::Lenient => upstream_error(&e),
                    },
                )?;
                let value: Option<Value> = match part {
                    Part::DistTags => Some(document.dist_tags().to_value()),
                    Part::Version(spec) => {
                        document.rewrite_tarballs(&package, &base);
                        document
                            .resolve(&spec)
                            .and_then(|version| document.version_value(&version))
                    }
                };
                Ok::<_, FormatError>(value.map(|v| Bytes::from(v.to_string())))
            })
            .await?
    }
}

/// What [`NpmHandler::render_document`] works from.
pub(crate) struct RenderSource {
    /// The stored or cached document.
    pub bytes: Bytes,
    /// How strictly to read it.
    pub mode: ParseMode,
    /// The package of the request.
    pub package: PackageName,
    /// The variant the client asked for.
    pub variant: Variant,
    /// The URL of the repository the client addressed.
    pub base: String,
    /// SHA-256 (hex) of `bytes`.
    pub digest: String,
}

/// A part of a document that has its own route.
pub(crate) enum Part {
    /// `GET /-/package/<package>/dist-tags`
    DistTags,
    /// `GET /<package>/<version or tag>`
    Version(String),
}
