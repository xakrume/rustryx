//! Checksum side files computed on demand: `<file>.sha1`, `<file>.sha256`, `<file>.md5`.
//!
//! The blob store already computed the digests while the bytes were written, so a side file
//! costs one metadata lookup and nothing is stored. The body is the bare lowercase hex digest
//! without a newline, the convention of Maven checksum files.
//!
//! Precedence: a file that was uploaded under the side file's own name always wins, because a
//! publisher may have uploaded `release.tar.gz.sha256` in whatever format their tooling
//! expects. The computed answer is only the fallback for a path with no file of that name,
//! and only for a base file that exists. Authorization: the caller needs `read` on the base
//! file as well, otherwise a path selector that allows `*.sha256` would hand out the digests
//! of files the caller may not read.

use axum_core::body::Body;
use http::{HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::identity::Action;
use rustryx_core::model::Asset;
use rustryx_core::ports::ResourceRef;
use rustryx_format_core::conditional::{Conditional, evaluate_conditional, format_http_date};
use rustryx_format_core::{ArtifactRequest, FormatContext, FormatError, NormalizedPath};

use crate::listing::map_auth_error;

/// A digest a side file can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Digest {
    /// `.sha1`
    Sha1,
    /// `.sha256`
    Sha256,
    /// `.md5`
    Md5,
}

impl Digest {
    /// The file name suffix, with the dot.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Sha1 => ".sha1",
            Self::Sha256 => ".sha256",
            Self::Md5 => ".md5",
        }
    }

    fn pick(self, asset: &Asset) -> &str {
        match self {
            Self::Sha1 => &asset.checksums.sha1,
            Self::Sha256 => &asset.checksums.sha256,
            Self::Md5 => &asset.checksums.md5,
        }
    }
}

/// The base file and the digest a path asks for, when it is a side file name.
///
/// `a/b.tgz.sha256` is `(a/b.tgz, Sha256)`. A name that is only the suffix (`.sha1`) has no
/// base and is an ordinary file name.
#[must_use]
pub fn sidecar_of(path: &NormalizedPath) -> Option<(NormalizedPath, Digest)> {
    let name = path.file_name()?;
    [Digest::Sha1, Digest::Sha256, Digest::Md5]
        .into_iter()
        .find_map(|digest| {
            let stem = name.strip_suffix(digest.suffix())?;
            if stem.is_empty() {
                return None;
            }
            let mut segments: Vec<&str> = path.segments().collect();
            segments.pop();
            segments.push(stem);
            NormalizedPath::from_segments(segments)
                .ok()
                .map(|base| (base, digest))
        })
}

/// Answers a side file from the stored digests of the base file, or `NotFound`.
pub(crate) async fn serve(
    cx: &FormatContext,
    req: &ArtifactRequest,
) -> Result<Response<Body>, FormatError> {
    let Some((base, digest)) = sidecar_of(&req.path) else {
        return Err(FormatError::NotFound);
    };
    let repository = cx.repository();
    let asset = cx
        .ports()
        .assets
        .get_by_path(repository.id, base.as_str())
        .await?
        .ok_or(FormatError::NotFound)?;
    let resource = ResourceRef::for_repository(repository, base.as_str());
    match cx
        .ports()
        .rbac
        .check(cx.principal(), Action::Read, &resource)
        .await
    {
        Ok(rustryx_core::ports::Decision::Allow) => {}
        // Not allowed to read the base: the side file does not exist for this caller.
        Ok(rustryx_core::ports::Decision::Deny) => return Err(FormatError::NotFound),
        Err(error) => return Err(map_auth_error(&error)),
    }
    Ok(respond(req, &asset, digest))
}

fn respond(req: &ArtifactRequest, asset: &Asset, digest: Digest) -> Response<Body> {
    let text = digest.pick(asset).to_owned();
    // The side file changes exactly when the base file does; the base's sha256 plus the
    // algorithm name identify it without hashing the answer again.
    let etag = format!("\"{}-{}\"", asset.checksums.sha256, &digest.suffix()[1..]);
    let method = &req.head.method;
    if matches!(
        evaluate_conditional(method, &req.head.headers, &etag, asset.updated_at),
        Conditional::NotModified
    ) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        decorate(&mut response, &etag, asset);
        return response;
    }
    let length = text.len();
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(text)
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    decorate(&mut response, &etag, asset);
    response
}

fn decorate(response: &mut Response<Body>, etag: &str, asset: &Asset) {
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format_http_date(asset.updated_at)) {
        headers.insert(header::LAST_MODIFIED, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(raw: &str) -> NormalizedPath {
        NormalizedPath::parse(raw).unwrap()
    }

    #[test]
    fn the_suffix_names_the_base_file_and_the_digest() {
        let (base, digest) = sidecar_of(&path("a/b/c.tgz.sha256")).unwrap();
        assert_eq!((base.as_str(), digest), ("a/b/c.tgz", Digest::Sha256));
        let (base, digest) = sidecar_of(&path("x.sha1")).unwrap();
        assert_eq!((base.as_str(), digest), ("x", Digest::Sha1));
        let (base, digest) = sidecar_of(&path("d/x.md5")).unwrap();
        assert_eq!((base.as_str(), digest), ("d/x", Digest::Md5));
    }

    #[test]
    fn other_names_are_not_side_files() {
        for raw in [
            "a/b.txt",
            "a/sha1",
            "a/.sha1",
            ".sha256",
            "a/b.sha512",
            "a/b.sha1x",
        ] {
            assert!(sidecar_of(&path(raw)).is_none(), "{raw}");
        }
        assert!(sidecar_of(&NormalizedPath::root()).is_none());
    }

    #[test]
    fn a_side_file_of_a_side_file_peels_one_suffix_only() {
        let (base, digest) = sidecar_of(&path("a.sha1.sha256")).unwrap();
        assert_eq!((base.as_str(), digest), ("a.sha1", Digest::Sha256));
    }
}
