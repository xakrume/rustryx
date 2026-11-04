//! Synchronize an RPM proxy: metadata first, then packages that are not cached yet.
//!
//! `POST /api/v1/repositories/{name}/actions/sync` starts one run. A second post while
//! that run is in progress returns the same progress. `DELETE` on the same path asks the
//! run to stop after the current batch. Packages already stored are skipped.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use http::{Method, StatusCode};
use rustryx_api::{AdminGrant, ApiError, Authorized, Requires, RouteModule, RouteSet};
use rustryx_core::identity::Principal;
use rustryx_core::model::{RepoFormat, RepoType};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{AssetRepo, RepositoryRepo};
use rustryx_format_core::{FormatEngine, RequestHead};
use rustryx_format_raw::rpm_index::{self, IndexError, RpmIndex};
use serde::Serialize;
use url::Url;

const INDEX_LIMIT: usize = 200 * 1024 * 1024;
const PARALLEL: usize = 4;

#[derive(Clone)]
struct SyncApi {
    engine: FormatEngine,
    repositories: Arc<dyn RepositoryRepo>,
    assets: Arc<dyn AssetRepo>,
    base_url: Url,
    runs: Arc<Mutex<std::collections::HashMap<String, SyncView>>>,
}

/// Routes that start and read an RPM sync.
pub struct RpmSyncModule {
    state: SyncApi,
}

impl RpmSyncModule {
    /// `base_url` is only the request head the format engine requires. Fetches go to the
    /// repository upstream, not to this URL.
    #[must_use]
    pub fn new(
        engine: FormatEngine,
        repositories: Arc<dyn RepositoryRepo>,
        assets: Arc<dyn AssetRepo>,
        base_url: Url,
    ) -> Self {
        Self {
            state: SyncApi {
                engine,
                repositories,
                assets,
                base_url,
                runs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            },
        }
    }
}

impl RouteModule for RpmSyncModule {
    fn name(&self) -> &'static str {
        "rpm-sync"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let state = self.state.clone();
        routes.route(
            "/api/v1/repositories/{name}/actions/sync",
            Requires::Admin,
            axum::routing::get(sync_status)
                .post(sync_start)
                .delete(sync_stop)
                .with_state(state),
        )
    }
}

#[derive(Clone, Serialize)]
struct SyncView {
    status: String,
    packages_total: u64,
    packages_skipped: u64,
    packages_fetched: u64,
    packages_failed: u64,
    error: Option<String>,
}

impl SyncView {
    fn idle() -> Self {
        Self {
            status: "idle".to_owned(),
            packages_total: 0,
            packages_skipped: 0,
            packages_fetched: 0,
            packages_failed: 0,
            error: None,
        }
    }

    fn running() -> Self {
        Self {
            status: "running".to_owned(),
            ..Self::idle()
        }
    }
}

async fn sync_status(
    State(api): State<SyncApi>,
    _proof: Authorized<AdminGrant>,
    Path(name): Path<String>,
) -> Result<Json<SyncView>, ApiError> {
    let _ = parse_name(&name)?;
    let runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
    Ok(Json(runs.get(&name).cloned().unwrap_or_else(SyncView::idle)))
}

async fn sync_start(
    State(api): State<SyncApi>,
    proof: Authorized<AdminGrant>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let repo_name = parse_name(&name)?;
    let repository = api
        .repositories
        .get_by_name(&repo_name)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "The repository was not found",
            )
        })?;
    if repository.format != RepoFormat::Rpm || repository.repo_type != RepoType::Proxy {
        return Err(ApiError::bad_request(
            "format_not_syncable",
            "Only an RPM proxy repository can be synchronized",
        ));
    }
    {
        let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        if runs
            .get(&name)
            .is_some_and(|view| view.status == "running" || view.status == "stopping")
        {
            let view = runs.get(&name).cloned().unwrap_or_else(SyncView::running);
            return Ok((StatusCode::OK, Json(view)).into_response());
        }
        runs.insert(name.clone(), SyncView::running());
    }
    let principal = proof.principal().clone();
    let api_task = api.clone();
    let repo = Arc::new(repository);
    let key = name.clone();
    tokio::spawn(async move {
        let result = run_sync(&api_task, &key, repo, principal).await;
        let mut runs = api_task
            .runs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let view = runs.entry(key).or_insert_with(SyncView::running);
        if view.status == "stopping" || view.status == "cancelled" {
            view.status = "cancelled".to_owned();
            view.error = None;
            return;
        }
        match result {
            Ok(()) if view.packages_failed == 0 => view.status = "succeeded".to_owned(),
            Ok(()) => {
                view.status = "failed".to_owned();
                view.error = Some("Some packages could not be fetched".to_owned());
            }
            Err(error) => {
                view.status = "failed".to_owned();
                view.error = Some(error);
            }
        }
    });
    let view = {
        let runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        runs.get(&name).cloned().unwrap_or_else(SyncView::running)
    };
    Ok((StatusCode::ACCEPTED, Json(view)).into_response())
}

async fn sync_stop(
    State(api): State<SyncApi>,
    _proof: Authorized<AdminGrant>,
    Path(name): Path<String>,
) -> Result<Json<SyncView>, ApiError> {
    let _ = parse_name(&name)?;
    let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(view) = runs.get_mut(&name)
        && view.status == "running"
    {
        view.status = "stopping".to_owned();
    }
    Ok(Json(runs.get(&name).cloned().unwrap_or_else(SyncView::idle)))
}

fn stopping(api: &SyncApi, name: &str) -> bool {
    let runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
    runs.get(name)
        .is_some_and(|view| view.status == "stopping" || view.status == "cancelled")
}

fn parse_name(name: &str) -> Result<RepoName, ApiError> {
    RepoName::parse(name).map_err(|_| {
        ApiError::bad_request("invalid_input", "The repository name is not valid")
    })
}

async fn run_sync(
    api: &SyncApi,
    name: &str,
    repository: Arc<rustryx_core::model::Repository>,
    principal: Principal,
) -> Result<(), String> {
    let repomd = read_path(api, &repository, &principal, "repodata/repomd.xml", INDEX_LIMIT).await?;
    let repomd_xml = String::from_utf8(repomd).map_err(|_| "repomd.xml is not UTF-8".to_owned())?;
    let metadata = rpm_index::metadata_paths(&repomd_xml).map_err(index_error)?;
    let primary = rpm_index::primary_path(&metadata)
        .ok_or_else(|| "repomd has no primary index".to_owned())?
        .to_owned();
    let primary_bytes = read_path(api, &repository, &principal, &primary, INDEX_LIMIT).await?;
    let primary_xml = String::from_utf8(rpm_index::inflate(&primary_bytes).map_err(index_error)?)
        .map_err(|_| "primary index is not UTF-8".to_owned())?;
    let index = RpmIndex {
        metadata,
        packages: rpm_index::package_paths(&primary_xml).map_err(index_error)?,
    };
    for path in &index.metadata {
        if stopping(api, name) {
            return Ok(());
        }
        if path == "repodata/repomd.xml" || path == &primary {
            continue;
        }
        if read_path(api, &repository, &principal, path, INDEX_LIMIT)
            .await
            .is_err()
        {
            note_failure(api, name);
        }
    }
    {
        let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(view) = runs.get_mut(name) {
            view.packages_total = u64::try_from(index.packages.len()).unwrap_or(u64::MAX);
        }
    }
    for chunk in index.packages.chunks(PARALLEL) {
        if stopping(api, name) {
            return Ok(());
        }
        let mut work = Vec::new();
        for path in chunk {
            if api
                .assets
                .get_by_path(repository.id, path)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                note_skipped(api, name);
                continue;
            }
            let api = api.clone();
            let repository = Arc::clone(&repository);
            let principal = principal.clone();
            let path = path.clone();
            let name = name.to_owned();
            work.push(async move {
                match fetch_package(&api, &repository, &principal, &path).await {
                    Ok(()) => note_fetched(&api, &name),
                    Err(_) => note_failure(&api, &name),
                }
            });
        }
        futures::future::join_all(work).await;
    }
    Ok(())
}

fn note_skipped(api: &SyncApi, name: &str) {
    let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(view) = runs.get_mut(name) {
        view.packages_skipped = view.packages_skipped.saturating_add(1);
    }
}

fn note_fetched(api: &SyncApi, name: &str) {
    let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(view) = runs.get_mut(name) {
        view.packages_fetched = view.packages_fetched.saturating_add(1);
    }
}

fn note_failure(api: &SyncApi, name: &str) {
    let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(view) = runs.get_mut(name) {
        view.packages_failed = view.packages_failed.saturating_add(1);
    }
}

async fn fetch_package(
    api: &SyncApi,
    repository: &Arc<rustryx_core::model::Repository>,
    principal: &Principal,
    path: &str,
) -> Result<(), String> {
    let response = serve(api, repository, principal, path).await;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status().as_u16()));
    }
    drain(response.into_body()).await
}

async fn read_path(
    api: &SyncApi,
    repository: &Arc<rustryx_core::model::Repository>,
    principal: &Principal,
    path: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let response = serve(api, repository, principal, path).await;
    if !response.status().is_success() {
        return Err(format!("{path} status {}", response.status().as_u16()));
    }
    let mut stream = response.into_body().into_data_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| format!("{path} could not be read"))?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(format!("{path} is larger than the index limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn serve(
    api: &SyncApi,
    repository: &Arc<rustryx_core::model::Repository>,
    principal: &Principal,
    path: &str,
) -> axum::response::Response {
    let head = RequestHead::new(
        Method::GET,
        path,
        principal.clone(),
        api.base_url.clone(),
    );
    api.engine
        .serve(Arc::clone(repository), head, Body::empty())
        .await
}

async fn drain(body: Body) -> Result<(), String> {
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        chunk.map_err(|_| "the package body could not be read".to_owned())?;
    }
    Ok(())
}

fn index_error(error: IndexError) -> String {
    match error {
        IndexError::Syntax => "the RPM index is not valid XML".to_owned(),
        IndexError::Path => "the RPM index contains a path outside the repository".to_owned(),
        IndexError::NoPrimary => "repomd has no primary index".to_owned(),
    }
}
