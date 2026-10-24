//! [`PypiHandler`]: the `FormatHandler` of the `PyPI` format.
//!
//! One handler serves hosted, proxy and group repositories. `classify` (in [`crate::route`]) runs
//! before the body is read and fixes the canonical path, the action and the coordinate;
//! `handle` parses the route from that canonical path and dispatches on the repository type.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderValue, Method, Response, header};
use moka::future::Cache;
use rustryx_core::identity::Action;
use rustryx_core::ids::BlobId;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::auth::Coordinate;
use rustryx_format_core::stream::{BoundedReadError, read_bounded};
use rustryx_format_core::{
    ArtifactRequest, ErrorInfo, FormatContext, FormatError, FormatHandler, GroupIndexMerger,
    GroupNameClaimer, NormalizedPath, RequestHead,
};

use crate::blocking::BlockingPool;
use crate::filename::DistFilename;
use crate::group::{PypiClaimer, PypiMerger};
use crate::name::ProjectName;
use crate::negotiate::Representation;
use crate::page::{ProjectPage, RootPage};
use crate::pages::{add_vary, negotiated, not_acceptable, page_response};
use crate::proxy::client_page;
use crate::route::{Route, SIMPLE, classify, parse_route};
use crate::sidecar::FileInfo;
use crate::{jsonapi, metrics, simple_html, simple_json};

/// Bounds and sizes of the `PyPI` handler. The defaults are the numbers the documentation states.
#[derive(Debug, Clone)]
pub struct PypiConfig {
    /// Most files in one project page. A project over it is answered 502 instead of a page that
    /// silently lacks files.
    pub max_index_files: usize,
    /// Most projects in the root index.
    pub max_root_projects: usize,
    /// Most components read to build the root index of a hosted repository.
    pub max_root_scan_components: usize,
    /// Index renders and parses that may run on the blocking pool at once.
    pub blocking_permits: usize,
    /// How long to wait for such a permit before answering 503.
    pub blocking_wait: Duration,
    /// Records of files read at once while a project page is built.
    pub sidecar_read_concurrency: usize,
    /// Entries of each cache of parsed records (per node).
    pub sidecar_cache_capacity: u64,
}

impl Default for PypiConfig {
    fn default() -> Self {
        Self {
            max_index_files: crate::page::MAX_FILES_PER_PAGE,
            max_root_projects: 100_000,
            max_root_scan_components: 500_000,
            blocking_permits: 8,
            blocking_wait: Duration::from_secs(5),
            sidecar_read_concurrency: 16,
            sidecar_cache_capacity: 100_000,
        }
    }
}

/// The `PyPI` format handler. Register one with the engine's `FormatRegistry`.
pub struct PypiHandler {
    pub(crate) config: PypiConfig,
    pub(crate) blocking: BlockingPool,
    /// Parsed `info.json` records, keyed by the blob that holds them: a blob never changes, so
    /// an entry cannot go stale (a rewritten record is a new blob).
    pub(crate) infos: Cache<BlobId, Arc<FileInfo>>,
    /// Yank reasons, keyed the same way.
    pub(crate) yanks: Cache<BlobId, Arc<str>>,
    merger: PypiMerger,
    claimer: PypiClaimer,
}

impl Default for PypiHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PypiHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PypiHandler")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl PypiHandler {
    /// A handler with the default bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(PypiConfig::default())
    }

    /// A handler with explicit bounds.
    #[must_use]
    pub fn with_config(config: PypiConfig) -> Self {
        Self {
            blocking: BlockingPool::new(config.blocking_permits, config.blocking_wait),
            infos: Cache::new(config.sidecar_cache_capacity),
            yanks: Cache::new(config.sidecar_cache_capacity),
            config,
            merger: PypiMerger,
            claimer: PypiClaimer,
        }
    }
}

/// True while a group resolves one of its members: the member answers in JSON for the merger, in
/// the one representation that carries every attribute.
fn is_member(cx: &FormatContext) -> bool {
    cx.origin_repository().id != cx.repository().id
}

fn source_label(cx: &FormatContext) -> &'static str {
    match cx.repository().repo_type {
        RepoType::Hosted => "hosted",
        RepoType::Proxy => "proxy",
        RepoType::Group => "group",
    }
}

/// URL of the repository the client addressed, for every URL written into a document.
fn origin_url(cx: &FormatContext, req: &ArtifactRequest) -> String {
    req.head.repository_url(&cx.origin_repository().name)
}

fn parse_failure() -> FormatError {
    FormatError::internal("a merged index document could not be read back")
}

/// The request a group needs to ask its members for a project page, whatever route the client
/// used: the JSON API of a group is built from the merged simple page.
fn simple_request(
    req: &ArtifactRequest,
    name: &ProjectName,
) -> Result<ArtifactRequest, FormatError> {
    let mut head: RequestHead = (*req.head).clone();
    head.method = Method::GET;
    head.path = format!("{SIMPLE}/{name}");
    for validator in [
        header::IF_NONE_MATCH,
        header::IF_MODIFIED_SINCE,
        header::RANGE,
    ] {
        head.headers.remove(validator);
    }
    head.headers.insert(
        header::ACCEPT,
        HeaderValue::from_static(Representation::Json.content_type()),
    );
    let path = NormalizedPath::from_segments([SIMPLE, name.as_str()])?;
    Ok(ArtifactRequest::new(&head, path, Action::Read)
        .with_coordinate(Coordinate::new(None, Some(name.to_string()), None))
        .index())
}

impl PypiHandler {
    async fn serve_root(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
    ) -> Result<Response<Body>, FormatError> {
        let member = is_member(cx);
        let representation = if member {
            Some(Representation::Json)
        } else {
            negotiated(&req.head)
        };
        let Some(representation) = representation else {
            return Ok(not_acceptable());
        };
        let source = source_label(cx);
        if cx.repository().repo_type == RepoType::Group {
            let mut response = cx.group().resolve(req).await?;
            add_vary(&mut response);
            metrics::index_served("root", source);
            return Ok(response);
        }
        let (root, document, complete) = if cx.repository().repo_type == RepoType::Hosted {
            let (root, truncated) = self.hosted_root(cx).await?;
            (root, None, !truncated)
        } else {
            let (mut root, document) = self.proxy_root(cx, req).await?;
            let cut = root.projects.len() > self.config.max_root_projects;
            root.projects.truncate(self.config.max_root_projects);
            (root, Some(document), !cut)
        };
        let body = self.blocking_render_root(root, representation).await?;
        Self::within_document_limit(cx, &body, source)?;
        metrics::index_served("root", source);
        let mut response = page_response(
            &req.head,
            representation.content_type(),
            !member,
            &body,
            document.as_ref(),
            complete,
        );
        if !complete {
            response.headers_mut().insert(
                "x-rustryx-index-truncated",
                HeaderValue::from_static("true"),
            );
        }
        Ok(response)
    }

    async fn serve_project(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
    ) -> Result<Response<Body>, FormatError> {
        let member = is_member(cx);
        let representation = if member {
            Some(Representation::Json)
        } else {
            negotiated(&req.head)
        };
        let Some(representation) = representation else {
            return Ok(not_acceptable());
        };
        let source = source_label(cx);
        if cx.repository().repo_type == RepoType::Group {
            let mut response = cx.group().resolve(req).await?;
            add_vary(&mut response);
            metrics::index_served("project", source);
            return Ok(response);
        }
        let (page, document) = self.own_page(cx, req, name).await?;
        let body = self.blocking_render_project(page, representation).await?;
        Self::within_document_limit(cx, &body, source)?;
        metrics::index_served("project", source);
        Ok(page_response(
            &req.head,
            representation.content_type(),
            !member,
            &body,
            document.as_ref(),
            true,
        ))
    }

    /// The page of a hosted or proxy repository, as the client sees it, with the proxy's cached
    /// upstream document when there is one.
    async fn own_page(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
    ) -> Result<(ProjectPage, Option<rustryx_format_core::CachedDocument>), FormatError> {
        let url = origin_url(cx, req);
        let (page, document) = if cx.repository().repo_type == RepoType::Hosted {
            (self.hosted_project_page(cx, name, &url).await?, None)
        } else {
            let (upstream, document) = self.proxy_upstream_page(cx, req, name).await?;
            (client_page(&upstream, name, &url), Some(document))
        };
        if page.files.len() > self.config.max_index_files {
            metrics::index_too_large(source_label(cx));
            return Err(FormatError::IndexTooLarge {
                limit: self.config.max_index_files as u64,
            });
        }
        Ok((page, document))
    }

    async fn blocking_render_project(
        &self,
        page: ProjectPage,
        representation: Representation,
    ) -> Result<Bytes, FormatError> {
        self.blocking
            .run(move || {
                Ok(Bytes::from(if representation.is_json() {
                    simple_json::render_project(&page)
                } else {
                    simple_html::render_project(&page)
                }))
            })
            .await
    }

    async fn blocking_render_root(
        &self,
        root: RootPage,
        representation: Representation,
    ) -> Result<Bytes, FormatError> {
        self.blocking
            .run(move || {
                Ok(Bytes::from(if representation.is_json() {
                    simple_json::render_root(&root)
                } else {
                    simple_html::render_root(&root)
                }))
            })
            .await
    }

    /// The explicit size cap of a generated document: `Limits::max_document_bytes`.
    fn within_document_limit(
        cx: &FormatContext,
        body: &Bytes,
        source: &'static str,
    ) -> Result<(), FormatError> {
        let limit = cx.limits().max_document_bytes;
        if body.len() as u64 > limit {
            metrics::index_too_large(source);
            return Err(FormatError::IndexTooLarge { limit });
        }
        Ok(())
    }

    async fn serve_file(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
        file: &DistFilename,
    ) -> Result<Response<Body>, FormatError> {
        match cx.repository().repo_type {
            RepoType::Hosted if *req.method() == Method::DELETE => {
                Self::delete_file(cx, name, file).await
            }
            RepoType::Hosted => cx.hosted().download(req).await,
            RepoType::Proxy => self.proxy_file(cx, req, name, file).await,
            RepoType::Group => cx.group().resolve(req).await,
        }
    }

    async fn serve_file_metadata(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
        file: &DistFilename,
    ) -> Result<Response<Body>, FormatError> {
        match cx.repository().repo_type {
            RepoType::Hosted => cx.hosted().download(req).await,
            RepoType::Proxy => self.proxy_metadata(cx, req, name, file).await,
            RepoType::Group => cx.group().resolve(req).await,
        }
    }

    async fn serve_json_api(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
        version: Option<&crate::version::Version>,
    ) -> Result<Response<Body>, FormatError> {
        let source = source_label(cx);
        let (page, document) = if cx.repository().repo_type == RepoType::Group {
            (self.group_page(cx, req, name).await?, None)
        } else {
            self.own_page(cx, req, name).await?
        };
        let version = version.cloned();
        let body = self
            .blocking
            .run(move || jsonapi::render(&page, version.as_ref()).map(Bytes::from))
            .await?;
        Self::within_document_limit(cx, &body, source)?;
        metrics::index_served("json_api", source);
        Ok(page_response(
            &req.head,
            "application/json",
            false,
            &body,
            document.as_ref(),
            true,
        ))
    }

    /// The merged page of a group, read back from its own JSON rendering.
    async fn group_page(
        &self,
        cx: &FormatContext,
        req: &ArtifactRequest,
        name: &ProjectName,
    ) -> Result<ProjectPage, FormatError> {
        let simple = simple_request(req, name)?;
        let response = cx.group().resolve(&simple).await?;
        let bytes = read_bounded(response.into_body(), cx.limits().max_document_bytes)
            .await
            .map_err(|error| match error {
                BoundedReadError::TooLarge { limit } => FormatError::IndexTooLarge { limit },
                BoundedReadError::Read(_) => parse_failure(),
            })?;
        let project = name.to_string();
        self.blocking
            .run(move || simple_json::parse_project(&bytes, &project).map_err(|_| parse_failure()))
            .await
    }
}

#[async_trait]
impl FormatHandler for PypiHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Pypi]
    }

    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        classify(repository, head)
    }

    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        // The route is read from the canonical path `classify` produced, never from the raw one.
        match parse_route(req.method(), &req.path)? {
            Route::Upload => self.upload(cx, req, body).await,
            Route::Root => self.serve_root(&cx, &req).await,
            Route::Project { name } => self.serve_project(&cx, &req, &name).await,
            Route::File { name, file } => self.serve_file(&cx, &req, &name, &file).await,
            Route::FileMetadata { name, file } => {
                self.serve_file_metadata(&cx, &req, &name, &file).await
            }
            Route::Yank { name, file } => Self::serve_yank(&cx, &req, &name, &file, body).await,
            Route::JsonApi { name, version } => {
                self.serve_json_api(&cx, &req, &name, version.as_ref())
                    .await
            }
        }
    }

    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        Some(&self.merger)
    }

    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        Some(&self.claimer)
    }

    fn render_error(&self, error: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
        crate::error::render(error, info)
    }
}
