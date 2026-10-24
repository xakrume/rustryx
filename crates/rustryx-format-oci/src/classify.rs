//! `classify`: the raw request path to a route, an action and a coordinate, before the body
//! is read.
//!
//! The path the engine receives is `v2/<image path>/<endpoint>` (the HTTP layer took the
//! Rustryx repository from the first segment of the URL below `/v2/`, or from
//! `/repository/<name>/`). It is decoded and validated once here, parsed into an
//! [`OciRoute`] and re-serialized: the canonical text is the normalized path that
//! authorization, routing rules and storage see, and `handle` parses THAT text again, never
//! the raw one.

use http::Method;
use rustryx_core::identity::Action;
use rustryx_core::ports::Coordinate;
use rustryx_format_core::{ArtifactRequest, FormatError, NormalizedPath, PathRules, RequestHead};

use crate::error::{OciErrorCode, coded};
use crate::parse::reference::ReferenceError;
use crate::parse::route::{OciRoute, RouteError};

/// A longer decoded path than this is no registry path: 255 for the image, a digest and the
/// words around them.
const MAX_PATH_LEN: usize = 512;

/// Classifies a request. See the module docs.
///
/// # Errors
/// `BadRequest` carrying an OCI error code for an invalid name, reference or digest,
/// `MethodNotAllowed` for a method the endpoint does not have, `NotFound` for a path that is
/// no endpoint.
pub fn classify_request(head: &RequestHead) -> Result<ArtifactRequest, FormatError> {
    let rules = PathRules {
        max_len: MAX_PATH_LEN,
        allow_trailing_slash: true,
        ..PathRules::default()
    };
    let normalized = NormalizedPath::parse_with(&head.path, &rules)
        .map_err(|_| bad(OciErrorCode::NameInvalid, "invalid path"))?;
    let below = normalized.strip_prefix("v2").ok_or(FormatError::NotFound)?;
    let route = OciRoute::parse(below.as_str()).map_err(|error| route_error(&error))?;
    let action = action_for(&route, &head.method)?;
    let path = NormalizedPath::parse(&route.canonical_path())
        .map_err(|_| FormatError::internal("a canonical registry path failed to normalize"))?;
    let mut request = ArtifactRequest::new(head, path, action);
    if let Some(image) = route.image() {
        request = request.with_coordinate(Coordinate::new(
            Some(image.as_str().to_owned()),
            Some(image.leaf().to_owned()),
            route.version(),
        ));
    }
    // Listings are documents a group merges and the metadata latency class is about;
    // manifests and blobs are artifacts (and count as downloads).
    if matches!(
        route,
        OciRoute::Tags { .. } | OciRoute::Referrers { .. } | OciRoute::Catalog
    ) {
        request = request.index();
    }
    Ok(request)
}

/// The action a method needs on a route, or `MethodNotAllowed`.
fn action_for(route: &OciRoute, method: &Method) -> Result<Action, FormatError> {
    let read = method == Method::GET || method == Method::HEAD;
    let allowed = |allow: &'static str| FormatError::MethodNotAllowed { allow };
    match route {
        OciRoute::Ping | OciRoute::Tags { .. } | OciRoute::Referrers { .. } => {
            if read {
                Ok(Action::Read)
            } else {
                Err(allowed("GET, HEAD"))
            }
        }
        OciRoute::Catalog => {
            if read {
                Ok(Action::Browse)
            } else {
                Err(allowed("GET, HEAD"))
            }
        }
        OciRoute::Manifest { .. } => match *method {
            Method::GET | Method::HEAD => Ok(Action::Read),
            Method::PUT => Ok(Action::Write),
            Method::DELETE => Ok(Action::Delete),
            _ => Err(allowed("GET, HEAD, PUT, DELETE")),
        },
        OciRoute::Blob { .. } => match *method {
            Method::GET | Method::HEAD => Ok(Action::Read),
            Method::DELETE => Ok(Action::Delete),
            _ => Err(allowed("GET, HEAD, DELETE")),
        },
        OciRoute::UploadStart { .. } => {
            if method == Method::POST {
                Ok(Action::Write)
            } else {
                Err(allowed("POST"))
            }
        }
        // Every method on a session needs write access: the session is the uploader's own,
        // and its state (the staged length) is not something a reader may see.
        OciRoute::Upload { .. } => match *method {
            Method::GET | Method::PATCH | Method::PUT | Method::DELETE => Ok(Action::Write),
            _ => Err(allowed("GET, PATCH, PUT, DELETE")),
        },
    }
}

fn route_error(error: &RouteError) -> FormatError {
    match error {
        RouteError::NotARoute => FormatError::NotFound,
        RouteError::Name(_) => bad(OciErrorCode::NameInvalid, "invalid repository name"),
        RouteError::Reference(ReferenceError::InvalidTag) => {
            bad(OciErrorCode::ManifestInvalid, "invalid tag")
        }
        RouteError::Reference(ReferenceError::InvalidDigest(_)) | RouteError::Digest(_) => {
            bad(OciErrorCode::DigestInvalid, "invalid digest")
        }
        RouteError::Session(_) => bad(OciErrorCode::BlobUploadInvalid, "invalid upload session"),
    }
}

fn bad(code: OciErrorCode, message: &str) -> FormatError {
    FormatError::bad_request(coded(code, message))
}

#[cfg(test)]
mod tests;
