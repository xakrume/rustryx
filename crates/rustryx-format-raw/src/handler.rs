//! [`RawHandler`]: the `FormatHandler` of the raw format.

use std::sync::Arc;

use async_trait::async_trait;
use axum_core::body::Body;
use http::{Method, Response};
use rustryx_core::identity::Action;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_format_core::{
    ArtifactRequest, ChecksumSource, FormatContext, FormatError, FormatHandler, ProxyFetch,
    RequestHead, StandardChecksumHeaders,
};

use crate::config::RawConfig;
use crate::target::{Target, parse_target};
use crate::{disposition, listing, sidecar, write};

/// Methods a file accepts.
const FILE_METHODS: &str = "GET, HEAD, PUT, DELETE";
/// Methods a directory accepts: it is read only.
const DIRECTORY_METHODS: &str = "GET, HEAD";

/// Plain files under `/repository/<name>/...`. Register one instance for the `raw` format.
#[derive(Debug, Clone, Copy)]
pub struct RawHandler {
    max_scanned: usize,
}

impl Default for RawHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl RawHandler {
    /// Creates the handler with the default listing scan bound
    /// ([`crate::listing::MAX_SCANNED`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_scanned: listing::MAX_SCANNED,
        }
    }

    /// Replaces the number of files one listing request may look at. A smaller bound makes a
    /// huge directory page sooner; the value is clamped to at least one file.
    #[must_use]
    pub fn with_scan_limit(mut self, max_scanned: usize) -> Self {
        self.max_scanned = max_scanned.max(1);
        self
    }
}

#[async_trait]
impl FormatHandler for RawHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        // RPM, apt and apk repositories are path trees. yum and dnf share `rpm`.
        // The same pipeline serves them until a format grows its own index.
        &[
            RepoFormat::Raw,
            RepoFormat::Rpm,
            RepoFormat::Apt,
            RepoFormat::Apk,
        ]
    }

    fn classify(
        &self,
        _repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        match parse_target(&head.path)? {
            Target::Directory(path) => {
                if !head.is_read_method() {
                    // A directory has no content to write or delete: the method does not
                    // apply to it, whatever the repository type.
                    return Err(FormatError::MethodNotAllowed {
                        allow: DIRECTORY_METHODS,
                    });
                }
                // A listing is a generated document, so it counts as metadata for the latency
                // objective; the group does not merge it (no merger), the first member wins.
                Ok(ArtifactRequest::new(head, path, Action::Browse).index())
            }
            Target::File(path) => {
                let action = match head.method {
                    Method::GET | Method::HEAD => Action::Read,
                    Method::PUT => Action::Write,
                    Method::DELETE => Action::Delete,
                    _ => {
                        return Err(FormatError::MethodNotAllowed {
                            allow: FILE_METHODS,
                        });
                    }
                };
                Ok(ArtifactRequest::new(head, path, action))
            }
        }
    }

    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let config = RawConfig::of(cx.repository());
        match req.action {
            Action::Browse => match cx.repository().repo_type {
                RepoType::Group => cx.group().resolve(&req).await,
                RepoType::Hosted | RepoType::Proxy => {
                    listing::serve(&cx, &req, self.max_scanned).await
                }
            },
            Action::Read => {
                let mut response = read_file(&cx, &req).await?;
                disposition::harden(&mut response, config, req.path.file_name());
                Ok(response)
            }
            Action::Write => write::put(&cx, &req, body).await,
            Action::Delete => write::delete(&cx, &req).await,
        }
    }
}

fn cached_component(format: RepoFormat, path: &str) -> Option<rustryx_format_core::ComponentSpec> {
    match format {
        RepoFormat::Rpm => crate::rpm_index::component_from_path(path),
        RepoFormat::Apt => crate::apt_index::component_from_path(path),
        RepoFormat::Apk => crate::apk_index::component_from_path(path),
        _ => None,
    }
}

/// A file read on a hosted, proxy or group repository, with the computed checksum side files
/// as the fallback for a path nothing is stored under.
async fn read_file(
    cx: &FormatContext,
    req: &ArtifactRequest,
) -> Result<Response<Body>, FormatError> {
    let result = match cx.repository().repo_type {
        RepoType::Hosted => cx.hosted().download(req).await,
        RepoType::Proxy => {
            // An upstream that announces `X-Checksum-*` headers gets its body verified before
            // a byte reaches the client; one that does not is streamed while it is cached.
            let source: Arc<dyn ChecksumSource> = Arc::new(StandardChecksumHeaders);
            let mut fetch = ProxyFetch::new(req).checksum_source(source);
            if let Some(component) = cached_component(cx.repository().format, req.path.as_str()) {
                fetch = fetch.component(component);
            }
            cx.proxy().fetch_artifact(fetch).await
        }
        RepoType::Group => return cx.group().resolve(req).await,
    };
    match result {
        // A group member asks the same question of itself, so the fallback needs no group
        // branch of its own; for a proxy it reads the cached base file and never fetches one
        // just to hash it.
        Err(FormatError::NotFound) => sidecar::serve(cx, req).await,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::identity::Principal;
    use rustryx_core::ids::{BlobStoreId, RepositoryId};
    use rustryx_core::names::RepoName;
    use url::Url;

    fn repository() -> Repository {
        Repository::new(
            RepositoryId::generate(),
            RepoName::parse("files").unwrap(),
            RepoFormat::Raw,
            RepoType::Hosted,
            BlobStoreId::generate(),
            time::OffsetDateTime::UNIX_EPOCH,
        )
    }

    fn classify(method: Method, path: &str) -> Result<ArtifactRequest, FormatError> {
        let head = RequestHead::new(
            method,
            path,
            Principal::anonymous(),
            Url::parse("https://x.example").unwrap(),
        );
        RawHandler::new().classify(&repository(), &head)
    }

    #[test]
    fn files_map_methods_to_actions() {
        for (method, action) in [
            (Method::GET, Action::Read),
            (Method::HEAD, Action::Read),
            (Method::PUT, Action::Write),
            (Method::DELETE, Action::Delete),
        ] {
            let req = classify(method.clone(), "a/b.txt").unwrap();
            assert_eq!(req.action, action, "{method}");
            assert_eq!(req.path.as_str(), "a/b.txt");
        }
    }

    #[test]
    fn directories_are_listed_and_never_written() {
        for path in ["", "/", "a/", "a/b/"] {
            let req = classify(Method::GET, path).unwrap();
            assert_eq!(req.action, Action::Browse, "{path}");
            assert_eq!(req.kind, rustryx_format_core::ContentKind::Index);
            assert!(classify(Method::HEAD, path).is_ok());
            for method in [Method::PUT, Method::DELETE, Method::POST] {
                match classify(method.clone(), path) {
                    Err(FormatError::MethodNotAllowed { allow }) => {
                        assert_eq!(allow, "GET, HEAD", "{method} {path}");
                    }
                    other => panic!("{method} {path:?}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn other_methods_on_a_file_name_the_allowed_ones() {
        for method in [Method::POST, Method::PATCH, Method::OPTIONS, Method::TRACE] {
            match classify(method.clone(), "a") {
                Err(FormatError::MethodNotAllowed { allow }) => {
                    assert_eq!(allow, "GET, HEAD, PUT, DELETE");
                }
                other => panic!("{method}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_bad_path_is_a_bad_request_before_the_method_is_looked_at() {
        for method in [Method::GET, Method::PUT, Method::DELETE] {
            assert!(matches!(
                classify(method, "a/../b"),
                Err(FormatError::BadRequest(_))
            ));
        }
    }

    #[test]
    fn the_handler_serves_raw_rpm_apt_and_apk() {
        assert_eq!(
            RawHandler::new().formats(),
            &[
                RepoFormat::Raw,
                RepoFormat::Rpm,
                RepoFormat::Apt,
                RepoFormat::Apk
            ]
        );
    }
}
