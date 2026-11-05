//! Synchronize an RPM, APT, or APK proxy: metadata first, then packages that are not cached yet.
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
use rustryx_core::audit::{AuditEvent, AuditKind};
use rustryx_core::identity::Principal;
use rustryx_core::model::{RepoFormat, RepoType};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{AssetRepo, AuditSink, RepositoryRepo};
use rustryx_format_core::{FormatEngine, RequestHead};
use rustryx_format_raw::apk_index;
use rustryx_format_raw::apt_index;
use rustryx_format_raw::rpm_index::{self, IndexError};
use serde::Serialize;
use url::Url;

const INDEX_LIMIT: usize = 200 * 1024 * 1024;
const PARALLEL: usize = 4;

#[derive(Clone)]
struct SyncApi {
    engine: FormatEngine,
    repositories: Arc<dyn RepositoryRepo>,
    assets: Arc<dyn AssetRepo>,
    audit: Arc<dyn AuditSink>,
    base_url: Url,
    runs: Arc<Mutex<std::collections::HashMap<String, SyncView>>>,
}

/// Routes that start and read a repository sync.
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
        audit: Arc<dyn AuditSink>,
        base_url: Url,
    ) -> Self {
        Self {
            state: SyncApi {
                engine,
                repositories,
                assets,
                audit,
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
    /// Bytes written while this run read package and metadata bodies.
    bytes_stored: u64,
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
            bytes_stored: 0,
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
    if repository.repo_type != RepoType::Proxy || !syncable(repository.format) {
        return Err(ApiError::bad_request(
            "format_not_syncable",
            "Only an RPM, APT, or APK proxy repository can be synchronized",
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
        let result = run_sync(&api_task, &key, Arc::clone(&repo), principal.clone()).await;
        let (outcome, detail) = {
            let mut runs = api_task
                .runs
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let view = runs.entry(key).or_insert_with(SyncView::running);
            let (outcome, error) = if view.status == "stopping" || view.status == "cancelled" {
                view.status = "cancelled".to_owned();
                view.error = None;
                ("success", None)
            } else {
                match result {
                    Ok(()) if view.packages_failed == 0 => {
                        view.status = "succeeded".to_owned();
                        ("success", None)
                    }
                    Ok(()) => {
                        view.status = "failed".to_owned();
                        view.error = Some("Some packages could not be fetched".to_owned());
                        ("failure", view.error.clone())
                    }
                    Err(error) => {
                        view.status = "failed".to_owned();
                        view.error = Some(error);
                        ("failure", view.error.clone())
                    }
                }
            };
            let detail = serde_json::json!({
                "status": view.status,
                "packages_total": view.packages_total,
                "packages_skipped": view.packages_skipped,
                "packages_fetched": view.packages_fetched,
                "packages_failed": view.packages_failed,
                "bytes_stored": view.bytes_stored,
                "error": error,
            });
            (outcome, detail)
        };
        record_sync(&api_task, &principal, &repo, outcome, detail).await;
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

async fn record_sync(
    api: &SyncApi,
    principal: &Principal,
    repository: &rustryx_core::model::Repository,
    outcome: &str,
    detail: serde_json::Value,
) {
    let mut event = AuditEvent::new(
        AuditKind::ContentChange,
        principal.username.clone(),
        "repository.sync",
        outcome,
    );
    event.actor_id = principal.user_id;
    event.actor_kind = principal.actor_kind();
    event.target = Some(repository.name.to_string());
    event.project_id = repository.project.as_ref().map(|project| project.id);
    event.detail = detail;
    if let Err(error) = api.audit.record(event).await {
        tracing::error!(%error, "sync audit record failed");
    }
}

fn syncable(format: RepoFormat) -> bool {
    matches!(format, RepoFormat::Rpm | RepoFormat::Apt | RepoFormat::Apk)
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
    record_sync(
        api,
        &principal,
        &repository,
        "success",
        serde_json::json!({ "status": "started" }),
    )
    .await;
    let packages = match repository.format {
        RepoFormat::Rpm => rpm_packages(api, &repository, &principal, name).await?,
        RepoFormat::Apt => apt_packages(api, &repository, &principal, name).await?,
        RepoFormat::Apk => apk_packages(api, &repository, &principal, name).await?,
        _ => return Err("this repository cannot be synchronized".to_owned()),
    };
    if stopping(api, name) {
        return Ok(());
    }
    {
        let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(view) = runs.get_mut(name) {
            view.packages_total = u64::try_from(packages.len()).unwrap_or(u64::MAX);
        }
    }
    for chunk in packages.chunks(PARALLEL) {
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
                match fetch_package(&api, &repository, &principal, &name, &path).await {
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

fn note_bytes(api: &SyncApi, name: &str, bytes: u64) {
    if bytes == 0 {
        return;
    }
    let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(view) = runs.get_mut(name) {
        view.bytes_stored = view.bytes_stored.saturating_add(bytes);
    }
}

async fn fetch_package(
    api: &SyncApi,
    repository: &Arc<rustryx_core::model::Repository>,
    principal: &Principal,
    name: &str,
    path: &str,
) -> Result<(), String> {
    let response = serve(api, repository, principal, path).await;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status().as_u16()));
    }
    count_body(api, name, response.into_body()).await
}

async fn read_path(
    api: &SyncApi,
    repository: &Arc<rustryx_core::model::Repository>,
    principal: &Principal,
    name: &str,
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
        note_bytes(api, name, u64::try_from(chunk.len()).unwrap_or(u64::MAX));
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

async fn count_body(api: &SyncApi, name: &str, body: Body) -> Result<(), String> {
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "the package body could not be read".to_owned())?;
        note_bytes(api, name, u64::try_from(chunk.len()).unwrap_or(u64::MAX));
    }
    Ok(())
}

async fn rpm_packages(
    api: &SyncApi,
    repository: &Arc<rustryx_core::model::Repository>,
    principal: &Principal,
    name: &str,
) -> Result<Vec<String>, String> {
    let repomd = read_path(
        api,
        repository,
        principal,
        name,
        "repodata/repomd.xml",
        INDEX_LIMIT,
    )
    .await?;
    let repomd_xml = String::from_utf8(repomd).map_err(|_| "repomd.xml is not UTF-8".to_owned())?;
    let metadata = rpm_index::metadata_paths(&repomd_xml).map_err(index_error)?;
    let primary = rpm_index::primary_path(&metadata)
        .ok_or_else(|| "repomd has no primary index".to_owned())?
        .to_owned();
    let primary_bytes = read_path(api, repository, principal, name, &primary, INDEX_LIMIT).await?;
    let primary_xml = String::from_utf8(rpm_index::inflate(&primary_bytes).map_err(index_error)?)
        .map_err(|_| "primary index is not UTF-8".to_owned())?;
    let packages = rpm_index::package_paths(&primary_xml).map_err(index_error)?;
    for path in &metadata {
        if stopping(api, name) {
            return Ok(Vec::new());
        }
        if path == "repodata/repomd.xml" || path == &primary {
            continue;
        }
        if read_path(api, repository, principal, name, path, INDEX_LIMIT)
            .await
            .is_err()
        {
            note_failure(api, name);
        }
    }
    Ok(packages)
}

async fn apt_packages(
    api: &SyncApi,
    repository: &Arc<rustryx_core::model::Repository>,
    principal: &Principal,
    name: &str,
) -> Result<Vec<String>, String> {
    let scope = mirror_scope(repository, &["amd64".to_owned()]);
    let mut packages = Vec::new();
    for distribution in &scope.distributions {
        if stopping(api, name) {
            return Ok(Vec::new());
        }
        let release_path = format!("dists/{distribution}/Release");
        let release_bytes = read_path(
            api,
            repository,
            principal,
            name,
            &release_path,
            INDEX_LIMIT,
        )
        .await?;
        let release = String::from_utf8(release_bytes)
            .map_err(|_| format!("{release_path} is not UTF-8"))?;
        let indexes = apt_index::package_indexes(&release, &scope.components, &scope.architectures)
            .map_err(apt_error)?;
        for index in indexes {
            if stopping(api, name) {
                return Ok(Vec::new());
            }
            let path = format!("dists/{distribution}/{index}");
            let bytes = read_path(api, repository, principal, name, &path, INDEX_LIMIT).await?;
            let text = String::from_utf8(apt_index::inflate(&bytes).map_err(apt_error)?)
                .map_err(|_| format!("{path} is not UTF-8"))?;
            packages.extend(apt_index::package_paths(&text).map_err(apt_error)?);
        }
    }
    Ok(packages)
}

async fn apk_packages(
    api: &SyncApi,
    repository: &Arc<rustryx_core::model::Repository>,
    principal: &Principal,
    name: &str,
) -> Result<Vec<String>, String> {
    let scope = mirror_scope(repository, &["x86_64".to_owned()]);
    let mut packages = Vec::new();
    for arch in &scope.architectures {
        if stopping(api, name) {
            return Ok(Vec::new());
        }
        let index_path = format!("{arch}/APKINDEX.tar.gz");
        let bytes = read_path(api, repository, principal, name, &index_path, INDEX_LIMIT).await?;
        let text = apk_index::apkindex_text(&bytes).map_err(apk_error)?;
        for file in apk_index::package_filenames(&text).map_err(apk_error)? {
            packages.push(format!("{arch}/{file}"));
        }
    }
    Ok(packages)
}

struct MirrorScope {
    distributions: Vec<String>,
    components: Vec<String>,
    architectures: Vec<String>,
}

fn mirror_scope(
    repository: &rustryx_core::model::Repository,
    default_architectures: &[String],
) -> MirrorScope {
    let config = &repository.format_config;
    let distributions = string_list(config, "distributions").unwrap_or_else(|| {
        string_list(config, "distribution").unwrap_or_else(|| default_distributions(repository))
    });
    let components = string_list(config, "components").unwrap_or_default();
    let architectures =
        string_list(config, "architectures").unwrap_or_else(|| default_architectures.to_vec());
    MirrorScope {
        distributions,
        components,
        architectures,
    }
}

fn default_distributions(repository: &rustryx_core::model::Repository) -> Vec<String> {
    let name = repository.name.as_str();
    if name.contains("ubuntu") {
        vec!["resolute".to_owned()]
    } else if name.contains("security") {
        vec!["stable-security".to_owned()]
    } else {
        vec!["stable".to_owned()]
    }
}

fn string_list(config: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let value = config.get(key)?;
    if let Some(text) = value.as_str() {
        let text = text.trim();
        return (!text.is_empty()).then(|| vec![text.to_owned()]);
    }
    let list = value.as_array()?;
    let items: Vec<String> = list
        .iter()
        .filter_map(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    (!items.is_empty()).then_some(items)
}

fn apt_error(error: apt_index::IndexError) -> String {
    match error {
        apt_index::IndexError::Syntax => "the APT index is not valid".to_owned(),
        apt_index::IndexError::Path => "the APT index contains a path outside the repository".to_owned(),
    }
}

fn apk_error(error: apk_index::IndexError) -> String {
    match error {
        apk_index::IndexError::Syntax => "the APK index is not valid".to_owned(),
        apk_index::IndexError::Path => "the APK index contains a path outside the repository".to_owned(),
    }
}

fn index_error(error: IndexError) -> String {
    match error {
        IndexError::Syntax => "the RPM index is not valid XML".to_owned(),
        IndexError::Path => "the RPM index contains a path outside the repository".to_owned(),
        IndexError::NoPrimary => "repomd has no primary index".to_owned(),
    }
}
