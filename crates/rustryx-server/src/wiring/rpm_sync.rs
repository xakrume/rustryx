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
use rustryx_core::error::StoreError;
use rustryx_core::ids::RepositoryId;
use rustryx_core::model::{RepoFormat, RepoType};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{AssetRepo, AuditSink, RepositoryRepo};
use rustryx_format_core::{FormatEngine, RequestHead};
use rustryx_format_raw::apk_index;
use rustryx_format_raw::apt_index;
use rustryx_format_raw::rpm_index::{self, IndexError};
use serde::{Deserialize, Serialize};
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
        routes
            .route(
                "/api/v1/repositories/{name}/actions/sync",
                Requires::Admin,
                axum::routing::get(sync_status)
                    .post(sync_start)
                    .delete(sync_stop)
                    .with_state(state.clone()),
            )
            .route(
                "/api/v1/repositories/{name}/actions/sync-filter",
                Requires::Admin,
                axum::routing::get(filter_get)
                    .put(filter_put)
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
    /// Paths left out by an include mask or an exclude pattern.
    packages_excluded: u64,
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
            packages_excluded: 0,
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
                "packages_excluded": view.packages_excluded,
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

#[derive(Debug, Deserialize, Serialize)]
struct SyncFilterBody {
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

async fn load_syncable(
    api: &SyncApi,
    name: &str,
) -> Result<rustryx_core::model::Repository, ApiError> {
    let repo_name = parse_name(name)?;
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
    Ok(repository)
}

async fn filter_get(
    State(api): State<SyncApi>,
    _proof: Authorized<AdminGrant>,
    Path(name): Path<String>,
) -> Result<Json<SyncFilterBody>, ApiError> {
    let repository = load_syncable(&api, &name).await?;
    let (include, exclude) = pattern_lists(&repository.format_config);
    Ok(Json(SyncFilterBody { include, exclude }))
}

async fn filter_put(
    State(api): State<SyncApi>,
    _proof: Authorized<AdminGrant>,
    Path(name): Path<String>,
    Json(body): Json<SyncFilterBody>,
) -> Result<Json<SyncFilterBody>, ApiError> {
    let mut repository = load_syncable(&api, &name).await?;
    let include = clean_patterns(&body.include)?;
    let exclude = clean_patterns(&body.exclude)?;
    write_patterns(&mut repository.format_config, &include, &exclude);
    api.repositories.update(&repository).await?;
    Ok(Json(SyncFilterBody { include, exclude }))
}

/// After a cached file is deleted, keep its path out of the next synchronization.
/// A repository that is not an RPM, APT, or APK proxy is left unchanged.
pub(super) async fn remember_sync_exclusion(
    repositories: &dyn RepositoryRepo,
    id: RepositoryId,
    path: &str,
) -> Result<(), StoreError> {
    let Some(mut repository) = repositories.get(id).await? else {
        return Ok(());
    };
    if repository.repo_type != RepoType::Proxy || !syncable(repository.format) {
        return Ok(());
    }
    if !push_exclusion(&mut repository.format_config, path) {
        return Ok(());
    }
    repositories.update(&repository).await
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
    let mut packages = match repository.format {
        RepoFormat::Rpm => rpm_packages(api, &repository, &principal, name).await?,
        RepoFormat::Apt => apt_packages(api, &repository, &principal, name).await?,
        RepoFormat::Apk => apk_packages(api, &repository, &principal, name).await?,
        _ => return Err("this repository cannot be synchronized".to_owned()),
    };
    if stopping(api, name) {
        return Ok(());
    }
    let (include, exclude) = pattern_lists(&repository.format_config);
    let excluded = packages
        .iter()
        .filter(|path| !path_selected(path, &include, &exclude))
        .count();
    packages.retain(|path| path_selected(path, &include, &exclude));
    {
        let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(view) = runs.get_mut(name) {
            view.packages_excluded = u64::try_from(excluded).unwrap_or(u64::MAX);
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
    if path_excluded(&repository.format_config, "repodata/repomd.xml") {
        return Err("repodata/repomd.xml is excluded".to_owned());
    }
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
    if path_excluded(&repository.format_config, &primary) {
        return Err(format!("{primary} is excluded"));
    }
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
        if path_excluded(&repository.format_config, path) {
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
        if path_excluded(&repository.format_config, &release_path) {
            return Err(format!("{release_path} is excluded"));
        }
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
            if path_excluded(&repository.format_config, &path) {
                continue;
            }
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
        if path_excluded(&repository.format_config, &index_path) {
            return Err(format!("{index_path} is excluded"));
        }
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

const PATTERN_LIMIT: usize = 200;
const PATTERN_LEN: usize = 256;

fn pattern_lists(config: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    (
        string_list(config, "sync_include").unwrap_or_default(),
        string_list(config, "sync_exclude").unwrap_or_default(),
    )
}

fn path_excluded(config: &serde_json::Value, path: &str) -> bool {
    let (_, exclude) = pattern_lists(config);
    exclude.iter().any(|pattern| glob_match(pattern, path))
}

/// An empty include list keeps every path. A pattern with no `/` also matches the file name.
fn path_selected(path: &str, include: &[String], exclude: &[String]) -> bool {
    let included = include.is_empty() || include.iter().any(|pattern| glob_match(pattern, path));
    included && !exclude.iter().any(|pattern| glob_match(pattern, path))
}

fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.contains('/') {
        return glob_bytes(pattern.as_bytes(), path.as_bytes());
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    glob_bytes(pattern.as_bytes(), path.as_bytes())
        || glob_bytes(pattern.as_bytes(), name.as_bytes())
}

fn glob_bytes(pattern: &[u8], text: &[u8]) -> bool {
    fn rec(pattern: &[u8], text: &[u8]) -> bool {
        if pattern.is_empty() {
            return text.is_empty();
        }
        if pattern[0] == b'*' {
            let rest = if pattern.len() > 1 && pattern[1] == b'*' {
                &pattern[2..]
            } else {
                &pattern[1..]
            };
            let double = pattern.len() > 1 && pattern[1] == b'*';
            if rec(rest, text) {
                return true;
            }
            if text.is_empty() {
                return false;
            }
            if !double && text[0] == b'/' {
                return false;
            }
            return rec(pattern, &text[1..]);
        }
        if text.is_empty() {
            return false;
        }
        if pattern[0] == b'?' {
            return text[0] != b'/' && rec(&pattern[1..], &text[1..]);
        }
        pattern[0] == text[0] && rec(&pattern[1..], &text[1..])
    }
    rec(pattern, text)
}

fn clean_patterns(patterns: &[String]) -> Result<Vec<String>, ApiError> {
    if patterns.len() > PATTERN_LIMIT {
        return Err(ApiError::bad_request(
            "invalid_input",
            "At most 200 sync patterns are allowed",
        ));
    }
    let mut cleaned = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() || pattern.len() > PATTERN_LEN || pattern.chars().any(char::is_control)
        {
            return Err(ApiError::bad_request(
                "invalid_input",
                "A sync pattern must be 1 to 256 characters without control characters",
            ));
        }
        if !cleaned.iter().any(|existing: &String| existing == pattern) {
            cleaned.push(pattern.to_owned());
        }
    }
    Ok(cleaned)
}

fn write_patterns(config: &mut serde_json::Value, include: &[String], exclude: &[String]) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }
    let object = config.as_object_mut().expect("format_config is an object");
    object.insert(
        "sync_include".to_owned(),
        serde_json::Value::from(include.to_vec()),
    );
    object.insert(
        "sync_exclude".to_owned(),
        serde_json::Value::from(exclude.to_vec()),
    );
}

/// Returns whether `path` was newly stored. A path already covered by a pattern is not repeated.
fn push_exclusion(config: &mut serde_json::Value, path: &str) -> bool {
    let path = path.trim().trim_start_matches('/');
    if path.is_empty() || path.len() > PATTERN_LEN || path.chars().any(char::is_control) {
        return false;
    }
    let (_, exclude) = pattern_lists(config);
    if exclude.iter().any(|pattern| glob_match(pattern, path)) {
        return false;
    }
    let mut exclude = exclude;
    exclude.push(path.to_owned());
    let (include, _) = pattern_lists(config);
    write_patterns(config, &include, &exclude);
    true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_without_a_slash_matches_the_file_name() {
        assert!(glob_match("*.rpm", "Packages/gitlab-ee.rpm"));
        assert!(!glob_match("*.rpm", "Packages/gitlab-ee.deb"));
        assert!(glob_match("*debuginfo*", "x86_64/foo-debuginfo-1.rpm"));
        assert!(glob_match("repodata/*", "repodata/repomd.xml"));
        assert!(!glob_match("repodata/*", "repodata/nested/file.xml"));
        assert!(glob_match("**/*.rpm", "a/b/c.rpm"));
        assert!(glob_match("foo?.rpm", "foox.rpm"));
        assert!(!glob_match("foo?.rpm", "foo/x.rpm"));
    }

    #[test]
    fn include_and_exclude_narrow_the_package_list() {
        let include = vec!["*.rpm".to_owned()];
        let exclude = vec!["*debuginfo*".to_owned()];
        assert!(path_selected("a/foo.rpm", &include, &exclude));
        assert!(!path_selected("a/foo-debuginfo.rpm", &include, &exclude));
        assert!(!path_selected("a/foo.deb", &include, &exclude));
        assert!(path_selected("a/foo.deb", &[], &[]));
    }

    #[test]
    fn a_deleted_path_is_stored_once_and_a_covered_path_is_not_repeated() {
        let mut config = serde_json::json!({ "distribution": "stable" });
        assert!(push_exclusion(&mut config, "/x86_64/foo.rpm"));
        assert!(!push_exclusion(&mut config, "x86_64/foo.rpm"));
        assert!(push_exclusion(&mut config, "x86_64/bar.rpm"));
        let patterns = config.get("sync_exclude").unwrap().as_array().unwrap();
        assert_eq!(patterns.len(), 2);
        assert_eq!(config.get("distribution").unwrap(), "stable");
        let mut covered = serde_json::json!({ "sync_exclude": ["*.rpm"] });
        assert!(!push_exclusion(&mut covered, "a/b.rpm"));
    }
}

fn index_error(error: IndexError) -> String {
    match error {
        IndexError::Syntax => "the RPM index is not valid XML".to_owned(),
        IndexError::Path => "the RPM index contains a path outside the repository".to_owned(),
        IndexError::NoPrimary => "repomd has no primary index".to_owned(),
    }
}
