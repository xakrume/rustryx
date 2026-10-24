//! Blob uploads: monolithic (`POST ...?digest=`), chunked (`POST`, `PATCH`..., `PUT`), status
//! and cancel. Sessions live in the blob store (format-core's chunked primitives), not in
//! this process, so any node can take the next request and no sticky sessions are needed.
//!
//! Every blob is verified against the digest the client declares while it is written (the
//! blob writer hashes in the same task as the write). A mismatch removes what was staged.

use axum_core::body::Body;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_format_core::{ChunkedSession, ExpectedChecksum, FormatError, HashAlgo, UploadSpec};

use crate::blobs;
use crate::error::{OciError, OciErrorCode};
use crate::headers::{DOCKER_UPLOAD_UUID, OCI_CHUNK_MIN_LENGTH, set_text, status_only};
use crate::mount;
use crate::ops::Op;
use crate::parse::digest::{Digest, DigestAlgorithm, DigestError};
use crate::parse::media_type::OCTET_STREAM;
use crate::parse::name::RepositoryName;
use crate::parse::session::UploadSessionId;
use crate::store;

/// `POST <name>/blobs/uploads/`: a mount, a monolithic upload or a new chunked session.
pub(crate) async fn start(
    op: &Op<'_>,
    name: &RepositoryName,
    body: Body,
) -> Result<Response<Body>, OciError> {
    let mount_digest = op.req.head.query_param("mount");
    let from = op.req.head.query_param("from");
    if let (Some(digest), Some(from)) = (&mount_digest, &from)
        && let Some(response) = mount::try_mount(op, name, digest, from).await?
    {
        return Ok(response);
    }
    match op.req.head.query_param("digest") {
        Some(digest) => monolithic(op, name, &digest, body).await,
        None => begin(op, name).await,
    }
}

/// `PATCH <name>/blobs/uploads/<session>`: appends a chunk.
pub(crate) async fn patch(
    op: &Op<'_>,
    name: &RepositoryName,
    session: &UploadSessionId,
    body: Body,
) -> Result<Response<Body>, OciError> {
    let staged = open_session(op, session)?;
    let declared = op.req.head.content_length();
    let offset = chunk_offset(op, &staged, declared).await?;
    match op
        .cx
        .hosted()
        .append_chunk(&staged, offset, body, declared)
        .await
    {
        Ok(length) => Ok(progress(op, name, session, length, StatusCode::ACCEPTED)),
        Err(FormatError::RangeNotSatisfiable { total }) => {
            // The client is out of step: tell it where the upload really is, so it can resume.
            Ok(progress(
                op,
                name,
                session,
                total,
                StatusCode::RANGE_NOT_SATISFIABLE,
            ))
        }
        Err(error) => Err(upload_error(error)),
    }
}

/// `PUT <name>/blobs/uploads/<session>?digest=`: the final chunk (optional) and the close.
pub(crate) async fn put(
    op: &Op<'_>,
    name: &RepositoryName,
    session: &UploadSessionId,
    body: Body,
) -> Result<Response<Body>, OciError> {
    let digest = declared_digest(op.req.head.query_param("digest").as_deref())?;
    let staged = open_session(op, session)?;
    let declared = op.req.head.content_length();
    if declared != Some(0) {
        let offset = chunk_offset(op, &staged, declared).await?;
        op.cx
            .hosted()
            .append_chunk(&staged, offset, body, declared)
            .await
            .map_err(upload_error)?;
    }
    let path = store::blob_path(name, &digest);
    if store::get_asset(op.cx, &path).await?.is_some() {
        // A blob is its digest: one that is already stored needs nothing from this upload,
        // and the stored copy was verified when it arrived. The staged chunks are dropped.
        op.cx.hosted().abort_chunked(staged).await?;
        return Ok(blobs::created(op, name, &digest));
    }
    let spec = blob_spec(&path, None, &digest)?;
    match op.cx.hosted().finish_chunked(staged, spec).await {
        Ok(_) => Ok(blobs::created(op, name, &digest)),
        Err(error) => Err(finish_error(error)),
    }
}

/// `GET <name>/blobs/uploads/<session>`: where the upload stands.
pub(crate) async fn status(
    op: &Op<'_>,
    name: &RepositoryName,
    session: &UploadSessionId,
) -> Result<Response<Body>, OciError> {
    let staged = open_session(op, session)?;
    let length = op
        .cx
        .hosted()
        .chunked_length(&staged)
        .await
        .map_err(upload_error)?;
    Ok(progress(op, name, session, length, StatusCode::NO_CONTENT))
}

/// `DELETE <name>/blobs/uploads/<session>`: cancels the upload.
pub(crate) async fn cancel(
    op: &Op<'_>,
    session: &UploadSessionId,
) -> Result<Response<Body>, OciError> {
    let staged = open_session(op, session)?;
    op.cx
        .hosted()
        .abort_chunked(staged)
        .await
        .map_err(upload_error)?;
    Ok(status_only(StatusCode::NO_CONTENT))
}

/// A new chunked session.
async fn begin(op: &Op<'_>, name: &RepositoryName) -> Result<Response<Body>, OciError> {
    if !op.cx.hosted().supports_chunked().await? {
        return Err(OciError::unsupported(
            StatusCode::NOT_IMPLEMENTED,
            "the blob store of this repository cannot stage resumable uploads; \
             push with a single request (POST with ?digest=)",
        ));
    }
    let staged = op.cx.hosted().begin_chunked().await?;
    let id = UploadSessionId::parse(&staged.token())
        .map_err(|_| OciError::internal("a session token is not a valid session id"))?;
    let mut response = progress(op, name, &id, 0, StatusCode::ACCEPTED);
    if let Some(min) = op.config().chunk_min_length {
        set_text(
            response.headers_mut(),
            OCI_CHUNK_MIN_LENGTH,
            &min.to_string(),
        );
    }
    Ok(response)
}

/// A whole blob in one request.
async fn monolithic(
    op: &Op<'_>,
    name: &RepositoryName,
    digest: &str,
    body: Body,
) -> Result<Response<Body>, OciError> {
    let digest = declared_digest(Some(digest))?;
    let path = store::blob_path(name, &digest);
    if store::get_asset(op.cx, &path).await?.is_some() {
        return Ok(blobs::created(op, name, &digest));
    }
    let spec = blob_spec(&path, op.req.head.content_length(), &digest)?;
    match op.cx.hosted().upload(spec, body).await {
        Ok(_) => Ok(blobs::created(op, name, &digest)),
        Err(error) => Err(finish_error(error)),
    }
}

/// The upload spec of a blob: its path, no component, and the digest it must hash to.
fn blob_spec(path: &str, length: Option<u64>, digest: &Digest) -> Result<UploadSpec, OciError> {
    let mut spec = UploadSpec::new(Op::path(path)?);
    spec.content_type = Some(OCTET_STREAM.to_owned());
    spec.content_length = length;
    spec.expected = Some(
        ExpectedChecksum::new(HashAlgo::Sha256, digest.hex())
            .map_err(|_| OciError::internal("a validated digest is not a valid checksum"))?,
    );
    Ok(spec)
}

/// The digest of a blob upload. Blobs are verified by the blob writer, which computes
/// SHA-256, so that is the one algorithm a blob may be addressed by.
fn declared_digest(text: Option<&str>) -> Result<Digest, OciError> {
    let text =
        text.ok_or_else(|| OciError::digest_invalid("the digest query parameter is required"))?;
    match Digest::parse(text) {
        Ok(digest) if digest.algorithm() == DigestAlgorithm::Sha256 => Ok(digest),
        Ok(_) | Err(DigestError::UnsupportedAlgorithm) => Err(OciError::digest_invalid(
            "only sha256 digests are supported for blobs",
        )),
        Err(DigestError::Malformed) => Err(OciError::digest_invalid("invalid digest")),
    }
}

/// The session a path names, if it is one of this repository's.
fn open_session(op: &Op<'_>, session: &UploadSessionId) -> Result<ChunkedSession, OciError> {
    ChunkedSession::parse(session.as_str(), op.cx.repository().id)
        .map_err(|_| OciError::upload_unknown())
}

/// Where a chunk starts: the first number of `Content-Range` when the client sent one (it
/// must match its own `Content-Length`), otherwise the length already staged.
async fn chunk_offset(
    op: &Op<'_>,
    staged: &ChunkedSession,
    declared: Option<u64>,
) -> Result<u64, OciError> {
    let range = op.header_values(&header::CONTENT_RANGE);
    match range.as_slice() {
        [] => op
            .cx
            .hosted()
            .chunked_length(staged)
            .await
            .map_err(upload_error),
        [value] => {
            let (start, end) = parse_content_range(value).ok_or_else(|| {
                OciError::new(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    OciErrorCode::BlobUploadInvalid,
                    "invalid Content-Range",
                )
            })?;
            if declared.is_some_and(|length| end - start + 1 != length) {
                return Err(OciError::size_invalid(
                    "Content-Range does not match Content-Length",
                ));
            }
            Ok(start)
        }
        _ => Err(OciError::upload_invalid(
            "more than one Content-Range header",
        )),
    }
}

/// `<start>-<end>`, both decimal, `start <= end` (the form the specification gives).
pub(crate) fn parse_content_range(value: &str) -> Option<(u64, u64)> {
    let (start, end) = value.trim().split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 19 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(start) || !digits(end) {
        return None;
    }
    let (start, end): (u64, u64) = (start.parse().ok()?, end.parse().ok()?);
    (start <= end).then_some((start, end))
}

/// The answer that reports an upload's progress: `Location`, `Range` (inclusive, `0-0` for
/// an empty upload), `Docker-Upload-UUID`.
fn progress(
    op: &Op<'_>,
    name: &RepositoryName,
    session: &UploadSessionId,
    length: u64,
    status: StatusCode,
) -> Response<Body> {
    let mut response = status_only(status);
    let headers = response.headers_mut();
    set_text(
        headers,
        header::LOCATION,
        &op.url(&format!("{name}/blobs/uploads/{session}")),
    );
    set_text(headers, DOCKER_UPLOAD_UUID, session.as_str());
    set_text(
        headers,
        header::RANGE,
        &format!("0-{}", length.saturating_sub(1)),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    response
}

/// Maps a failure while writing chunks: an unknown session, a bad body.
fn upload_error(error: FormatError) -> OciError {
    match error {
        FormatError::NotFound => OciError::upload_unknown(),
        other => OciError::from(other).bad_request_as(OciErrorCode::BlobUploadInvalid),
    }
}

/// Maps a failure while finishing an upload: a digest that does not match is the one the
/// client needs to hear about by name.
fn finish_error(error: FormatError) -> OciError {
    match error {
        FormatError::DigestMismatch => OciError::digest_invalid(
            "the digest of the uploaded content does not match the declared digest",
        ),
        other => upload_error(other),
    }
}

#[cfg(test)]
mod tests;
