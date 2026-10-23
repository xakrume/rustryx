//! The default flow for plain, path-addressed formats (raw, and the file-like
//! parts of maven or helm): classify by method, serve by repository type.
//!
//! A real format uses these as building blocks and overrides what is special about
//! it. The test format in this crate's integration tests is nothing but these two
//! functions.

use axum_core::body::Body;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_core::identity::Action;
use rustryx_core::model::{RepoType, Repository};

use crate::conditional::etag_from_checksums;
use crate::context::FormatContext;
use crate::error::FormatError;
use crate::hosted::UploadSpec;
use crate::path::NormalizedPath;
use crate::proxy::ProxyFetch;
use crate::request::{ArtifactRequest, RequestHead, action_for_method};

/// Classifies a request whose URL path is the artifact path: the method gives the
/// action, the path is normalized with the default rules (no traversal, no empty
/// segments, no trailing slash), there is no coordinate and the kind is
/// [`crate::ContentKind::Artifact`].
///
/// # Errors
/// `BadRequest` for an invalid path, `MethodNotAllowed` for a method that is not a
/// content operation.
pub fn classify_path_addressed(
    _repository: &Repository,
    head: &RequestHead,
) -> Result<ArtifactRequest, FormatError> {
    let action = action_for_method(&head.method).ok_or(FormatError::MethodNotAllowed {
        allow: "GET, HEAD, PUT, DELETE",
    })?;
    let path = NormalizedPath::parse(&head.path)?;
    Ok(ArtifactRequest::new(head, path, action))
}

impl FormatContext {
    /// Serves a classified request according to the repository type, with the default
    /// behaviour of every pipeline:
    ///
    /// | type | read | write | delete |
    /// |---|---|---|---|
    /// | hosted | download | streamed upload, `201` (new) or `200` (replaced) | `204` |
    /// | proxy | cached fetch from the upstream | 405 | 405 |
    /// | group | first member that has it | 405 | 405 |
    ///
    /// Cancel safe, see the pipelines.
    ///
    /// # Errors
    /// Whatever the pipeline returns.
    pub async fn serve_default(
        &self,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let read = matches!(req.action, Action::Read | Action::Browse);
        match (self.repository().repo_type, read, req.action) {
            (RepoType::Hosted, true, _) => self.hosted().download(&req).await,
            (RepoType::Hosted, false, Action::Delete) => {
                self.hosted().delete(&req.path).await?;
                Ok(status_only(StatusCode::NO_CONTENT))
            }
            (RepoType::Hosted, false, _) => {
                let outcome = self
                    .hosted()
                    .upload(UploadSpec::from_request(&req), body)
                    .await?;
                let mut response = status_only(if outcome.created() {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                });
                if let Ok(etag) =
                    HeaderValue::from_str(&etag_from_checksums(&outcome.asset.checksums))
                {
                    response.headers_mut().insert(header::ETAG, etag);
                }
                Ok(response)
            }
            (RepoType::Proxy, true, _) => self.proxy().fetch_artifact(ProxyFetch::new(&req)).await,
            (RepoType::Group, true, _) => self.group().resolve(&req).await,
            (RepoType::Proxy | RepoType::Group, false, _) => {
                Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" })
            }
        }
    }
}

fn status_only(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Method;
    use rustryx_core::identity::Principal;
    use url::Url;

    fn head(method: Method, path: &str) -> RequestHead {
        RequestHead::new(
            method,
            path,
            Principal::anonymous(),
            Url::parse("https://x.example").unwrap(),
        )
    }

    fn repo() -> Repository {
        use rustryx_core::ids::{BlobStoreId, RepositoryId};
        use rustryx_core::model::RepoFormat;
        Repository::new(
            RepositoryId::generate(),
            rustryx_core::names::RepoName::parse("r").unwrap(),
            RepoFormat::Raw,
            RepoType::Hosted,
            BlobStoreId::generate(),
            time::OffsetDateTime::UNIX_EPOCH,
        )
    }

    #[test]
    fn classifies_by_method_and_normalizes_the_path() {
        for (method, action) in [
            (Method::GET, Action::Read),
            (Method::HEAD, Action::Read),
            (Method::PUT, Action::Write),
            (Method::DELETE, Action::Delete),
        ] {
            let req = classify_path_addressed(&repo(), &head(method, "a/b%20c.txt")).unwrap();
            assert_eq!(req.action, action);
            assert_eq!(req.path.as_str(), "a/b c.txt");
        }
    }

    #[test]
    fn rejects_unknown_methods_and_bad_paths() {
        assert!(matches!(
            classify_path_addressed(&repo(), &head(Method::OPTIONS, "a")),
            Err(FormatError::MethodNotAllowed { .. })
        ));
        for bad in ["../x", "a//b", "a/%2e%2e/b", "a%2fb"] {
            assert!(
                matches!(
                    classify_path_addressed(&repo(), &head(Method::GET, bad)),
                    Err(FormatError::BadRequest(_))
                ),
                "{bad}"
            );
        }
    }
}
