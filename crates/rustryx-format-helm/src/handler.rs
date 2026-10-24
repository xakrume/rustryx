//! [`HelmHandler`]: the `FormatHandler` of a classic Helm chart repository.

use async_trait::async_trait;
use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::identity::Action;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::auth::Coordinate;
use rustryx_format_core::conditional::etag_from_checksums;
use rustryx_format_core::hosted::{ComponentSpec, UploadSpec};
use rustryx_format_core::stream::{BoundedReadError, read_bounded};
use rustryx_format_core::{
    ArtifactRequest, FormatContext, FormatError, FormatHandler, GroupIndexMerger, GroupNameClaimer,
    NormalizedPath, ProxyFetch, RequestHead,
};

use crate::blocking::{BlockingPool, PARSE_PERMITS, PARSE_WAIT};
use crate::chart::{ChartLimits, read_chart};
use crate::group::{HelmClaimer, HelmMerger};
use crate::index::{self, DEFAULT_MAX_CHARTS, chart_failure};
use crate::names::archive_file_name;
use crate::response::{self, INDEX_TYPE};
use crate::route::{self, Target};

/// Classic Helm chart repositories (`index.yaml` plus `charts/*.tgz`).
///
/// Register one instance for [`RepoFormat::Helm`]. Hosted, proxy and group repositories are
/// all served. See `docs/format-helm.md`.
#[derive(Debug, Clone)]
pub struct HelmHandler {
    pool: BlockingPool,
    limits: ChartLimits,
    max_index_charts: usize,
    merger: HelmMerger,
    claimer: HelmClaimer,
}

impl Default for HelmHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl HelmHandler {
    /// A handler with the default chart limits and index cap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pool: BlockingPool::new(PARSE_PERMITS, PARSE_WAIT),
            limits: ChartLimits::default(),
            max_index_charts: DEFAULT_MAX_CHARTS,
            merger: HelmMerger,
            claimer: HelmClaimer,
        }
    }

    /// Replaces the archive limits. The upload body is buffered up to `max_archive_bytes`
    /// because the chart reader validates the whole archive before anything is stored.
    #[must_use]
    pub fn with_chart_limits(mut self, limits: ChartLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replaces how many charts one generated index may list. Values below one become one.
    #[must_use]
    pub fn with_max_index_charts(mut self, max_charts: usize) -> Self {
        self.max_index_charts = max_charts.max(1);
        self
    }
}

const CHART_ALLOW: &str = "GET, HEAD, PUT, DELETE";
const INDEX_READ_ALLOW: &str = "GET, HEAD";

fn action_of(method: &Method, allow: &'static str) -> Result<Action, FormatError> {
    if method == Method::GET || method == Method::HEAD {
        Ok(Action::Read)
    } else if method == Method::PUT {
        Ok(Action::Write)
    } else if method == Method::DELETE {
        Ok(Action::Delete)
    } else {
        Err(FormatError::MethodNotAllowed { allow })
    }
}

#[async_trait]
impl FormatHandler for HelmHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Helm]
    }

    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        let path = NormalizedPath::parse(&head.path)?;
        let target = route::classify_path(&path, repository.repo_type)?;
        let allow = match &target {
            Target::Index => INDEX_READ_ALLOW,
            Target::Chart(_) | Target::LooseChart(_) => CHART_ALLOW,
        };
        // `PUT` and `DELETE` of `index.yaml` are classified so authorization runs. `handle`
        // refuses them: the index is generated, never stored.
        let action = action_of(&head.method, allow)?;
        let mut request = ArtifactRequest::new(head, path, action);
        if let Target::Chart(file) = &target {
            request = request.with_coordinate(Coordinate::new(
                None,
                Some(file.name.clone()),
                Some(file.version.clone()),
            ));
        }
        if matches!(target, Target::Index) {
            request = request.index();
        }
        Ok(request)
    }

    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let target = route::interpret(req.path.as_str()).ok_or_else(|| {
            FormatError::bad_request("the path is not a helm chart repository path")
        })?;
        match (cx.repository().repo_type, &target, req.action) {
            (_, Target::Index, Action::Write) => Err(FormatError::bad_request(
                "index.yaml is generated and cannot be uploaded",
            )),
            (_, _, Action::Browse) => Err(FormatError::MethodNotAllowed { allow: CHART_ALLOW }),
            (RepoType::Hosted, Target::Index, Action::Read) => self.hosted_index(&cx, &req).await,
            (RepoType::Hosted, Target::Chart(_file), Action::Read) => {
                cx.hosted().download(&req).await
            }
            (RepoType::Hosted, Target::Chart(file), Action::Write) => {
                self.hosted_put(&cx, &req, file, body).await
            }
            (RepoType::Hosted, Target::Chart(_), Action::Delete) => {
                self.hosted_delete(&cx, &req).await
            }
            (RepoType::Hosted, Target::LooseChart(_), Action::Read) => Err(FormatError::NotFound),
            (RepoType::Proxy, Target::Index, Action::Read) => self.proxy_index(&cx, &req).await,
            (RepoType::Proxy, Target::Chart(_) | Target::LooseChart(_), Action::Read) => {
                cx.proxy().fetch_artifact(ProxyFetch::new(&req)).await
            }
            (
                RepoType::Group,
                Target::Index | Target::Chart(_) | Target::LooseChart(_),
                Action::Read,
            ) => cx.group().resolve(&req).await,
            (_, Target::Index, Action::Delete) | (RepoType::Proxy | RepoType::Group, _, _) => {
                Err(FormatError::MethodNotAllowed {
                    allow: INDEX_READ_ALLOW,
                })
            }
            (RepoType::Hosted, Target::LooseChart(_), _) => {
                Err(FormatError::bad_request(crate::route::BAD_CHART_FILE))
            }
        }
    }

    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        Some(&self.merger)
    }

    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        Some(&self.claimer)
    }
}

impl HelmHandler {
    async fn hosted_index(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
    ) -> Result<Response<Body>, FormatError> {
        let base = req.head.repository_url(&cx.origin_repository().name);
        let index =
            index::generate(cx, &self.pool, &self.limits, self.max_index_charts, &base).await?;
        Ok(response::document(
            &req.head.method,
            index::to_bytes(&index)?,
            INDEX_TYPE,
            HeaderMap::new(),
        ))
    }

    async fn hosted_put(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        file: &crate::names::ChartFile,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        let archive = read_upload(body, &req.head, self.limits.max_archive_bytes).await?;
        let limits = self.limits;
        let parsed = archive.clone();
        let info = self
            .pool
            .run(move || read_chart(&parsed, &limits))
            .await?
            .map_err(|error| chart_failure(error, limits.max_archive_bytes))?;
        let canonical = archive_file_name(&info.metadata.name, &info.metadata.version);
        if file.file_name() != canonical {
            return Err(FormatError::bad_request(
                "the chart archive does not match the file name",
            ));
        }
        let mut spec = UploadSpec::from_request(req);
        spec.content_type = Some("application/gzip".to_owned());
        spec.content_length = Some(u64_len(archive.len()));
        spec.component = Some(ComponentSpec {
            group: None,
            name: info.metadata.name,
            version: Some(info.metadata.version),
        });
        let outcome = cx.hosted().upload(spec, Body::from(archive)).await?;
        let status = if outcome.created() {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        let mut response = Response::new(Body::empty());
        *response.status_mut() = status;
        if let Ok(etag) = HeaderValue::from_str(&etag_from_checksums(&outcome.asset.checksums)) {
            response.headers_mut().insert(header::ETAG, etag);
        }
        Ok(response)
    }

    async fn hosted_delete(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
    ) -> Result<Response<Body>, FormatError> {
        cx.hosted().delete(&req.path).await?;
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NO_CONTENT;
        Ok(response)
    }

    async fn proxy_index(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
    ) -> Result<Response<Body>, FormatError> {
        let fetched = cx
            .proxy()
            .fetch_document(ProxyFetch::new(req).content_type(INDEX_TYPE))
            .await?;
        let mut index = index::parse_index(&fetched.body, || {
            FormatError::upstream(
                rustryx_format_core::UpstreamFailureKind::BadStatus,
                "upstream index.yaml is not valid",
            )
        })?;
        let base = req.head.repository_url(&cx.origin_repository().name);
        index::rewrite_urls(&mut index, &base);
        let mut extra = HeaderMap::new();
        extra.insert(
            rustryx_format_core::hosted::X_RUSTRYX_CACHE,
            HeaderValue::from_static(fetched.cache.as_str()),
        );
        Ok(response::document(
            &req.head.method,
            index::to_bytes(&index)?,
            INDEX_TYPE,
            extra,
        ))
    }
}

async fn read_upload(body: Body, head: &RequestHead, limit: u64) -> Result<Bytes, FormatError> {
    if head.content_length().is_some_and(|length| length > limit) {
        return Err(FormatError::PayloadTooLarge { limit });
    }
    read_bounded(body, limit)
        .await
        .map_err(|error| match error {
            BoundedReadError::TooLarge { limit } => FormatError::PayloadTooLarge { limit },
            BoundedReadError::Read(_) => {
                FormatError::bad_request("the request body could not be read")
            }
        })
}

fn u64_len(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::identity::Principal;
    use rustryx_core::ids::{BlobStoreId, RepositoryId};
    use rustryx_core::names::RepoName;
    use url::Url;

    fn repository(repo_type: RepoType) -> Repository {
        Repository::new(
            RepositoryId::generate(),
            RepoName::parse("charts").unwrap(),
            RepoFormat::Helm,
            repo_type,
            BlobStoreId::generate(),
            time::OffsetDateTime::UNIX_EPOCH,
        )
    }

    fn classify(
        repo_type: RepoType,
        method: Method,
        path: &str,
    ) -> Result<ArtifactRequest, FormatError> {
        let head = RequestHead::new(
            method,
            path,
            Principal::anonymous(),
            Url::parse("https://x.example").unwrap(),
        );
        HelmHandler::new().classify(&repository(repo_type), &head)
    }

    #[test]
    fn the_handler_serves_helm_only() {
        assert_eq!(HelmHandler::new().formats(), &[RepoFormat::Helm]);
        assert_eq!(
            HelmHandler::new().with_max_index_charts(0).max_index_charts,
            1
        );
        assert_eq!(DEFAULT_MAX_CHARTS, 4096);
    }

    #[test]
    fn methods_map_to_actions_and_the_index_is_metadata() {
        let index = classify(RepoType::Hosted, Method::GET, "index.yaml").unwrap();
        assert_eq!(index.action, Action::Read);
        assert_eq!(index.kind, rustryx_format_core::ContentKind::Index);
        let chart = classify(RepoType::Hosted, Method::PUT, "charts/web-1.2.3.tgz").unwrap();
        assert_eq!(chart.action, Action::Write);
        assert_eq!(
            chart.coordinate.as_ref().and_then(|c| c.name.as_deref()),
            Some("web")
        );
        assert_eq!(
            chart.coordinate.as_ref().and_then(|c| c.version.as_deref()),
            Some("1.2.3")
        );
        assert!(matches!(
            classify(RepoType::Hosted, Method::POST, "charts/web-1.2.3.tgz"),
            Err(FormatError::MethodNotAllowed { allow: CHART_ALLOW })
        ));
    }

    #[test]
    fn path_traversal_is_a_bad_request_before_the_method_is_considered() {
        for method in [Method::GET, Method::PUT, Method::DELETE] {
            assert!(
                matches!(
                    classify(RepoType::Hosted, method.clone(), "charts/../index.yaml"),
                    Err(FormatError::BadRequest(message)) if message == "invalid path"
                ),
                "{method}"
            );
        }
    }

    #[test]
    fn put_of_index_is_still_a_write_so_authorization_can_run() {
        let request = classify(RepoType::Hosted, Method::PUT, "index.yaml").unwrap();
        assert_eq!(request.action, Action::Write);
        assert_eq!(request.kind, rustryx_format_core::ContentKind::Index);
    }
}
