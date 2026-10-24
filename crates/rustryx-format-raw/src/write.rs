//! `PUT` and `DELETE` of a raw file on a hosted repository.
//!
//! The one rule this file adds to the shared hosted pipeline: **a path is either a file or a
//! directory prefix, never both.** An upload is refused with `409` when
//!
//! * an ancestor of the path is a file (`a` is a file, the upload is `a/b`), or
//! * the path is a directory prefix (`a/b` has files below it, the upload is `a/b`).
//!
//! The check runs before the body is read. It is a read of the current state, not a lock:
//! two uploads of `a` and `a/b` that race, on one node or two, can both pass it, because the
//! asset port has no multi path constraint to put it in. The invariant stays safe to read
//! regardless: a request addresses a file or a directory by its trailing slash (see
//! [`crate::target`]), so a repository that does hold both answers `GET a` with the file and
//! `GET a/` with the listing, and nothing can be made to mean the other.

use axum_core::body::Body;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::model::Asset;
use rustryx_core::ports::PageRequest;
use rustryx_format_core::conditional::{Conditional, etag_from_checksums, evaluate_conditional};
use rustryx_format_core::{
    ArtifactRequest, FormatContext, FormatError, NormalizedPath, OverwriteRule, UploadSpec,
};

/// Stores the request body at the request path.
pub(crate) async fn put(
    cx: &FormatContext,
    req: &ArtifactRequest,
    body: Body,
) -> Result<Response<Body>, FormatError> {
    let repository = cx.repository();
    let existing = cx
        .ports()
        .assets
        .get_by_path(repository.id, req.path.as_str())
        .await?;
    check_preconditions(&req.head.headers, existing.as_ref())?;
    if existing.is_none() {
        // An existing file at this path proves the ancestors are directories and that there
        // is nothing below it (the invariant held when it was written), so the walk is only
        // needed for a new path.
        ensure_no_conflict(cx, &req.path).await?;
    }
    let mut spec = UploadSpec::from_request(req);
    if creates_only(&req.head.headers) {
        // `If-None-Match: *` is checked above against what this request saw; the create-only
        // publish makes the database refuse a writer that lost a race, on any node.
        spec.overwrite = OverwriteRule::Never;
    }
    let outcome = cx.hosted().upload(spec, body).await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = if outcome.created() {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(0u64));
    if let Ok(etag) = HeaderValue::from_str(&etag_from_checksums(&outcome.asset.checksums)) {
        headers.insert(header::ETAG, etag);
    }
    Ok(response)
}

/// Deletes the file at the request path (`404` when there is none).
pub(crate) async fn delete(
    cx: &FormatContext,
    req: &ArtifactRequest,
) -> Result<Response<Body>, FormatError> {
    cx.hosted().delete(&req.path).await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    Ok(response)
}

fn creates_only(headers: &HeaderMap) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim() == "*")
}

/// Evaluates `If-Match`, `If-None-Match` and the date conditions of a write against the file
/// the request saw (or the absence of one). `If-Match` needs a file; `If-None-Match: *` needs
/// none.
fn check_preconditions(headers: &HeaderMap, existing: Option<&Asset>) -> Result<(), FormatError> {
    match existing {
        None => {
            if headers.contains_key(header::IF_MATCH) {
                return Err(FormatError::PreconditionFailed);
            }
            Ok(())
        }
        Some(asset) => {
            let etag = etag_from_checksums(&asset.checksums);
            match evaluate_conditional(&Method::PUT, headers, &etag, asset.updated_at) {
                Conditional::Proceed => Ok(()),
                // `NotModified` is only produced for safe methods; for a write any failing
                // condition is a failed precondition.
                Conditional::PreconditionFailed | Conditional::NotModified => {
                    Err(FormatError::PreconditionFailed)
                }
            }
        }
    }
}

/// Refuses a path that would be both a file and a directory.
async fn ensure_no_conflict(cx: &FormatContext, path: &NormalizedPath) -> Result<(), FormatError> {
    let repository = cx.repository();
    let mut ancestor = String::new();
    let segments: Vec<&str> = path.segments().collect();
    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        if !ancestor.is_empty() {
            ancestor.push('/');
        }
        ancestor.push_str(segment);
        if cx
            .ports()
            .assets
            .get_by_path(repository.id, &ancestor)
            .await?
            .is_some()
        {
            return Err(FormatError::conflict(
                "a parent of this path is a file, so it cannot also be a directory",
            ));
        }
    }
    let below = cx
        .ports()
        .assets
        .list(
            repository.id,
            &format!("{path}/"),
            &PageRequest {
                limit: 1,
                after: None,
            },
        )
        .await?;
    if below.items.is_empty() {
        Ok(())
    } else {
        Err(FormatError::conflict(
            "this path is a directory, so it cannot also be a file",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_testkit::fixtures;

    fn asset() -> Asset {
        let blob = fixtures::blob_record("k", b"content");
        Asset::from_blob(
            rustryx_core::ids::AssetId::generate(),
            rustryx_core::ids::RepositoryId::generate(),
            "a",
            "text/plain",
            &blob,
            time::OffsetDateTime::UNIX_EPOCH,
        )
    }

    fn headers(pairs: &[(http::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn if_none_match_star_needs_a_missing_file() {
        let star = headers(&[(header::IF_NONE_MATCH, "*")]);
        assert!(check_preconditions(&star, None).is_ok());
        assert!(matches!(
            check_preconditions(&star, Some(&asset())),
            Err(FormatError::PreconditionFailed)
        ));
        assert!(creates_only(&star));
        assert!(!creates_only(&headers(&[(header::IF_NONE_MATCH, "\"x\"")])));
    }

    #[test]
    fn if_match_needs_the_current_etag() {
        let current = asset();
        let etag = etag_from_checksums(&current.checksums);
        let good = headers(&[(header::IF_MATCH, &etag)]);
        assert!(check_preconditions(&good, Some(&current)).is_ok());
        assert!(check_preconditions(&headers(&[(header::IF_MATCH, "*")]), Some(&current)).is_ok());
        let stale = headers(&[(header::IF_MATCH, "\"other\"")]);
        assert!(matches!(
            check_preconditions(&stale, Some(&current)),
            Err(FormatError::PreconditionFailed)
        ));
        assert!(matches!(
            check_preconditions(&good, None),
            Err(FormatError::PreconditionFailed)
        ));
    }

    #[test]
    fn a_write_without_conditions_proceeds_either_way() {
        let none = HeaderMap::new();
        assert!(check_preconditions(&none, None).is_ok());
        assert!(check_preconditions(&none, Some(&asset())).is_ok());
    }

    #[test]
    fn a_matching_if_none_match_etag_refuses_the_overwrite() {
        let current = asset();
        let etag = etag_from_checksums(&current.checksums);
        let same = headers(&[(header::IF_NONE_MATCH, &etag)]);
        assert!(matches!(
            check_preconditions(&same, Some(&current)),
            Err(FormatError::PreconditionFailed)
        ));
    }
}
