//! Small helpers on [`ArtifactRequest`].

use axum_core::body::Body;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_format_core::{ArtifactRequest, FormatError, NormalizedPath};

/// The same request for another path of the same repository. The new path goes through the
/// same normalization as the original, so authorization decided on the original covers it only
/// because both name the same artifact (a snapshot pointer and the build it resolves to, a
/// checksum file and the file it describes).
pub(crate) fn with_path(req: &ArtifactRequest, path: &str) -> Result<ArtifactRequest, FormatError> {
    Ok(ArtifactRequest {
        path: NormalizedPath::parse(path)?,
        ..req.clone()
    })
}

/// A body-less response with this status.
pub(crate) fn status_only(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

/// `201 Created`, or `200 OK` when the path existed, with the strong `ETag` of the content.
pub(crate) fn upload_response(created: bool, etag: &str) -> Response<Body> {
    let mut response = status_only(if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    });
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}
