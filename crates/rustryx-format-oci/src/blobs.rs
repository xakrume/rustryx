//! Blobs of a hosted repository: read, delete, and the response a finished upload gives.

use axum_core::body::Body;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_format_core::FormatError;

use crate::error::{OciError, OciErrorCode};
use crate::headers::{DOCKER_CONTENT_DIGEST, set_text, stamp_version, status_only};
use crate::ops::Op;
use crate::parse::digest::Digest;
use crate::parse::name::RepositoryName;
use crate::store;

/// `GET` and `HEAD` of a blob: the stored object, with ranges, streamed.
pub(crate) async fn read(op: &Op<'_>, digest: &Digest) -> Result<Response<Body>, OciError> {
    match op.cx.hosted().download(op.req).await {
        Ok(mut response) => {
            stamp(&mut response, digest);
            Ok(response)
        }
        Err(error) => Err(OciError::from(error)
            .not_found_as(OciErrorCode::BlobUnknown, "blob unknown to registry")),
    }
}

/// `DELETE` of a blob: removes the repository's link to it. The object itself goes with the
/// garbage collector when no asset references it any more.
pub(crate) async fn delete(op: &Op<'_>) -> Result<Response<Body>, OciError> {
    if store::get_asset(op.cx, op.req.path.as_str())
        .await?
        .is_none()
    {
        return Err(OciError::blob_unknown());
    }
    match op.cx.hosted().delete(&op.req.path).await {
        // `None` is "the blob is still referenced elsewhere", which is a success here.
        Ok(_) => Ok(status_only(StatusCode::ACCEPTED)),
        Err(FormatError::NotFound) => Err(OciError::blob_unknown()),
        Err(error) => Err(error.into()),
    }
}

/// The answer to a finished upload or a mount: 201 with the blob's own URL.
pub(crate) fn created(op: &Op<'_>, name: &RepositoryName, digest: &Digest) -> Response<Body> {
    let mut response = status_only(StatusCode::CREATED);
    let headers = response.headers_mut();
    set_text(
        headers,
        header::LOCATION,
        &op.url(&format!("{name}/blobs/{digest}")),
    );
    set_text(headers, DOCKER_CONTENT_DIGEST, &digest.to_string());
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    response
}

/// Adds `Docker-Content-Digest` and the version header to a served blob.
fn stamp(response: &mut Response<Body>, digest: &Digest) {
    let headers = response.headers_mut();
    set_text(headers, DOCKER_CONTENT_DIGEST, &digest.to_string());
    stamp_version(headers);
}
