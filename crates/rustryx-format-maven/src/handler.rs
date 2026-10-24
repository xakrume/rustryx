//! [`MavenHandler`]: the `FormatHandler` of the Maven 2 repository format.

use async_trait::async_trait;
use axum_core::body::Body;
use http::{HeaderValue, Method, Response, header};
use rustryx_core::identity::Action;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::auth::Coordinate;
use rustryx_format_core::{
    ArtifactRequest, ErrorInfo, FormatContext, FormatError, FormatHandler, GroupIndexMerger,
    GroupNameClaimer, NormalizedPath, PathRules, RequestHead,
};

use crate::blocking::BlockingPool;
use crate::config::{MavenConfig, VersionPolicy};
use crate::group::{MavenClaimer, MavenMerger};
use crate::layout::{MavenPath, parse_version_dir};
use crate::metrics;

/// The Maven 2 format.
///
/// Register one instance in the `FormatRegistry`; it serves hosted, proxy and group
/// repositories of `RepoFormat::Maven2`. See `docs/format-maven.md` for the protocol surface.
#[derive(Debug)]
pub struct MavenHandler {
    pub(crate) config: MavenConfig,
    pub(crate) pool: BlockingPool,
    merger: MavenMerger,
    claimer: MavenClaimer,
}

impl Default for MavenHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl MavenHandler {
    /// A handler with the default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(MavenConfig::default())
    }

    /// A handler with explicit settings.
    #[must_use]
    pub fn with_config(config: MavenConfig) -> Self {
        Self {
            pool: BlockingPool::new(config.blocking_permits, config.permit_wait),
            config,
            merger: MavenMerger,
            claimer: MavenClaimer,
        }
    }
}

/// Methods Maven clients use. `POST` and `PATCH` are not part of the protocol.
const ALLOW: &str = "GET, HEAD, PUT, DELETE";

fn action_of(method: &Method) -> Result<Action, FormatError> {
    if method == Method::GET || method == Method::HEAD {
        Ok(Action::Read)
    } else if method == Method::PUT {
        Ok(Action::Write)
    } else if method == Method::DELETE {
        Ok(Action::Delete)
    } else {
        Err(FormatError::MethodNotAllowed { allow: ALLOW })
    }
}

fn layout_refusal() -> FormatError {
    metrics::rejection("layout");
    FormatError::bad_request("the path is not a Maven 2 repository layout path")
}

#[async_trait]
impl FormatHandler for MavenHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Maven2]
    }

    fn classify(
        &self,
        _repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        let action = action_of(&head.method)?;
        // A trailing slash is a directory: it means something only for the delete of a version,
        // and nothing (404) for a read. A write to a directory is refused.
        let rules = PathRules {
            allow_trailing_slash: action != Action::Write,
            ..PathRules::default()
        };
        let path = NormalizedPath::parse_with(&head.path, &rules)?;
        let request = ArtifactRequest::new(head, path.clone(), action);
        if path.has_trailing_slash() {
            return Ok(match (action, parse_version_dir(path.as_str())) {
                (Action::Delete, Ok((group, artifact, version))) => request
                    .with_coordinate(Coordinate::new(Some(group), Some(artifact), Some(version))),
                (Action::Delete, Err(_)) => return Err(layout_refusal()),
                _ => request,
            });
        }
        match MavenPath::parse(path.as_str()) {
            Ok(parsed) => {
                let request = request.with_coordinate(parsed.coordinate());
                Ok(if matches!(parsed, MavenPath::Metadata { .. }) {
                    request.index()
                } else {
                    request
                })
            }
            // A read of a path outside the layout is allowed through (a proxy forwards
            // `archetype-catalog.xml`); it carries no coordinate, so a content selector on a
            // coordinate does not match it. Anything that changes state must be a layout path.
            Err(_) if action == Action::Read => Ok(request),
            Err(_) => Err(layout_refusal()),
        }
    }

    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let parsed = if req.path.has_trailing_slash() {
            None
        } else {
            MavenPath::parse(req.path.as_str()).ok()
        };
        match (cx.repository().repo_type, req.action) {
            (RepoType::Hosted, Action::Write) => {
                let parsed = parsed.ok_or_else(layout_refusal)?;
                self.hosted_put(&cx, &req, parsed, body).await
            }
            (RepoType::Hosted, Action::Delete) => self.hosted_delete(&cx, &req, parsed).await,
            (RepoType::Hosted, _) => self.hosted_get(&cx, &req, parsed).await,
            (RepoType::Proxy, Action::Read | Action::Browse) => {
                self.proxy_get(&cx, &req, parsed).await
            }
            (RepoType::Group, Action::Read | Action::Browse) => {
                self.group_get(&cx, &req, parsed).await
            }
            (RepoType::Proxy | RepoType::Group, _) => {
                Err(FormatError::MethodNotAllowed { allow: "GET, HEAD" })
            }
        }
    }

    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        Some(&self.merger)
    }

    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        Some(&self.claimer)
    }

    /// Errors are plain text: that is what Maven's transports and compat answer with, and a
    /// Maven client reads the status line, not the body. A 401 carries a Basic challenge because
    /// Maven sends credentials only after it was challenged (a `GET` has no preemptive
    /// authentication by default).
    fn render_error(&self, error: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
        let mut response = Response::new(Body::from(format!("{}\n", error.public_message())));
        *response.status_mut() = error.status();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.extend(error.headers());
        if error.status() == http::StatusCode::UNAUTHORIZED {
            headers.insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"Rustryx\""),
            );
        }
        if let Some(id) = info.request_id
            && let Ok(value) = HeaderValue::from_str(id)
        {
            headers.insert("x-request-id", value);
        }
        response
    }
}

/// Checks the repository's version policy for a write, before the body is read.
pub(crate) fn enforce_write_policy(
    repository: &Repository,
    version: &str,
) -> Result<(), FormatError> {
    let policy = VersionPolicy::of(repository)?;
    if policy.allows(version) {
        Ok(())
    } else {
        metrics::rejection("version_policy");
        Err(FormatError::bad_request(policy.refusal()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::identity::Principal;
    use rustryx_core::ids::{BlobStoreId, RepositoryId};
    use url::Url;

    fn repository() -> Repository {
        Repository::new(
            RepositoryId::generate(),
            rustryx_core::names::RepoName::parse("m").unwrap(),
            RepoFormat::Maven2,
            RepoType::Hosted,
            BlobStoreId::generate(),
            time::OffsetDateTime::UNIX_EPOCH,
        )
    }

    fn head(method: Method, path: &str) -> RequestHead {
        RequestHead::new(
            method,
            path,
            Principal::anonymous(),
            Url::parse("https://x.test").unwrap(),
        )
    }

    fn classify(method: Method, path: &str) -> Result<ArtifactRequest, FormatError> {
        MavenHandler::new().classify(&repository(), &head(method, path))
    }

    #[test]
    fn a_file_read_has_a_coordinate_and_is_not_an_index() {
        let request = classify(Method::GET, "org/acme/lib/1.0/lib-1.0.jar").unwrap();
        assert_eq!(request.action, Action::Read);
        assert_eq!(request.kind, rustryx_format_core::ContentKind::Artifact);
        let coordinate = request.coordinate.unwrap();
        assert_eq!(coordinate.name.as_deref(), Some("lib"));
    }

    #[test]
    fn metadata_and_its_checksums_are_index_documents() {
        for path in [
            "org/acme/lib/maven-metadata.xml",
            "org/acme/lib/maven-metadata.xml.sha1",
            "org/acme/lib/1.0-SNAPSHOT/maven-metadata.xml",
        ] {
            let request = classify(Method::GET, path).unwrap();
            assert_eq!(
                request.kind,
                rustryx_format_core::ContentKind::Index,
                "{path}"
            );
        }
    }

    #[test]
    fn deleting_a_directory_carries_the_version_coordinate() {
        let request = classify(Method::DELETE, "org/acme/lib/1.0/").unwrap();
        assert_eq!(request.action, Action::Delete);
        let coordinate = request.coordinate.unwrap();
        assert_eq!(coordinate.version.as_deref(), Some("1.0"));
        assert_eq!(coordinate.extension, None);
        assert!(matches!(
            classify(Method::DELETE, "lib/1.0/"),
            Err(FormatError::BadRequest(_))
        ));
    }

    #[test]
    fn a_directory_read_has_no_coordinate_and_a_directory_write_is_refused() {
        let read = classify(Method::GET, "org/acme/lib/1.0/").unwrap();
        assert!(read.coordinate.is_none());
        assert!(matches!(
            classify(Method::PUT, "org/acme/lib/1.0/"),
            Err(FormatError::BadRequest(_))
        ));
    }

    #[test]
    fn a_read_outside_the_layout_passes_and_a_write_does_not() {
        let read = classify(Method::GET, "archetype-catalog.xml").unwrap();
        assert!(read.coordinate.is_none());
        for method in [Method::PUT, Method::DELETE] {
            assert!(matches!(
                classify(method, "archetype-catalog.xml"),
                Err(FormatError::BadRequest(_))
            ));
        }
    }

    #[test]
    fn unsupported_methods_and_bad_paths_are_refused() {
        for method in [Method::POST, Method::PATCH, Method::OPTIONS, Method::TRACE] {
            assert!(matches!(
                classify(method, "org/acme/lib/1.0/lib-1.0.jar"),
                Err(FormatError::MethodNotAllowed { .. })
            ));
        }
        assert!(matches!(
            classify(Method::GET, "org/../etc/passwd"),
            Err(FormatError::BadRequest(_))
        ));
    }

    #[test]
    fn the_handler_serves_maven2_only() {
        assert_eq!(MavenHandler::default().formats(), &[RepoFormat::Maven2]);
        assert!(MavenHandler::new().group_merge().is_some());
        assert!(MavenHandler::new().group_claims().is_some());
        assert!(MavenHandler::new().hide_inaccessible_repository());
    }

    fn render(error: &FormatError, request_id: Option<&str>) -> Response<Body> {
        let repository = repository();
        MavenHandler::new().render_error(
            error,
            &ErrorInfo {
                request_id,
                repository: &repository,
                anonymous: true,
            },
        )
    }

    #[test]
    fn a_401_carries_the_basic_challenge_and_the_request_id() {
        let response = render(&FormatError::Unauthenticated, Some("r-1"));
        assert_eq!(response.status(), 401);
        assert_eq!(
            response.headers()[header::WWW_AUTHENTICATE],
            "Basic realm=\"Rustryx\""
        );
        assert_eq!(response.headers()["x-request-id"], "r-1");
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/plain")
        );
    }

    #[test]
    fn protocol_headers_of_the_error_are_kept() {
        let response = render(&FormatError::MethodNotAllowed { allow: "GET, HEAD" }, None);
        assert_eq!(response.headers()[header::ALLOW], "GET, HEAD");
        assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
        let response = render(&FormatError::RangeNotSatisfiable { total: 9 }, None);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */9");
        let response = render(
            &FormatError::Unavailable {
                reason: "busy",
                retry_after: Some(std::time::Duration::from_secs(3)),
            },
            None,
        );
        assert_eq!(response.headers()[header::RETRY_AFTER], "3");
    }

    #[test]
    fn the_write_policy_check_names_the_refused_kind() {
        let mut repository = repository();
        repository.format_config = serde_json::json!({"version_policy": "release"});
        assert!(enforce_write_policy(&repository, "1.0").is_ok());
        match enforce_write_policy(&repository, "1.0-SNAPSHOT") {
            Err(FormatError::BadRequest(message)) => assert!(message.contains("release")),
            other => panic!("{other:?}"),
        }
    }
}
