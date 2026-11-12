//! Cleanup policies, lifecycle and GC history, tasks and settings.
//!
//! The tables and ports already exist. These routes are what the web UI opens
//! after the menu lists. A cleanup preview records a finished dry run with zero
//! candidates: nothing is deleted. Settings sections are the values from the
//! service configuration, so the form can render them and cannot change them.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::header::ETAG;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http::{HeaderMap, StatusCode};
use rustryx_api::{
    AdminGrant, ApiError, ApiJson, ApiPath, ApiQuery, Authorized, Requires, RouteModule, RouteSet,
};
use rustryx_core::ids::{CleanupPolicyId, GcRunId, LifecycleRunId};
use rustryx_core::lifecycle::{
    CleanupCriteria, CleanupPolicy, GcRun, ItemDecision, LifecycleItem, LifecycleKind,
    LifecycleRun, ReleaseType,
};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{
    CleanupPolicyRepo, Clock, GcRunRepo, LifecycleRunRepo, PageRequest, PolicyRef, RepositoryRepo,
};
use rustryx_core::tasks::TaskRunStatus;
use rustryx_db::repos::cleanup::PgCleanupPolicyRepo;
use rustryx_db::repos::gc::PgGcRunRepo;
use rustryx_db::repos::lifecycle::PgLifecycleRunRepo;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use sqlx::types::Uuid;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Ports the operations pages share.
pub struct OpsDeps {
    /// Metadata pool. Task rows have no repository type yet, so those reads use SQL.
    pub pool: sqlx::PgPool,
    /// Repository names for a policy's attachments.
    pub repositories: Arc<dyn RepositoryRepo>,
    /// Wall clock for a preview that finishes immediately.
    pub clock: Arc<dyn Clock>,
}

/// Operations routes.
pub struct OpsModule {
    state: OpsApi,
}

impl OpsModule {
    /// Builds the module.
    #[must_use]
    pub fn new(deps: OpsDeps) -> Self {
        let pool = deps.pool;
        Self {
            state: OpsApi {
                cleanup: Arc::new(PgCleanupPolicyRepo::new(pool.clone())),
                runs: Arc::new(PgLifecycleRunRepo::new(pool.clone())),
                gc: Arc::new(PgGcRunRepo::new(pool.clone())),
                repositories: deps.repositories,
                clock: deps.clock,
                pool,
            },
        }
    }
}

impl RouteModule for OpsModule {
    fn name(&self) -> &'static str {
        "ops"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let state = self.state.clone();
        routes
            .route(
                "/api/v1/cleanup-policies",
                Requires::Admin,
                get(list_policies)
                    .post(create_policy)
                    .with_state(state.clone()),
            )
            .route(
                "/api/v1/cleanup-policies/{policy_id}",
                Requires::Admin,
                get(get_policy)
                    .put(update_policy)
                    .delete(delete_policy)
                    .with_state(state.clone()),
            )
            .route(
                "/api/v1/cleanup-policies/{policy_id}/actions/preview",
                Requires::Admin,
                post(preview_policy).with_state(state.clone()),
            )
            .route(
                "/api/v1/lifecycle-runs",
                Requires::Admin,
                get(list_lifecycle).with_state(state.clone()),
            )
            .route(
                "/api/v1/lifecycle-runs/{run_id}",
                Requires::Admin,
                get(get_lifecycle).with_state(state.clone()),
            )
            .route(
                "/api/v1/lifecycle-runs/{run_id}/candidates",
                Requires::Admin,
                get(lifecycle_candidates).with_state(state.clone()),
            )
            .route(
                "/api/v1/gc/runs",
                Requires::Admin,
                get(list_gc).with_state(state.clone()),
            )
            .route(
                "/api/v1/gc/runs/{run_id}",
                Requires::Admin,
                get(get_gc).with_state(state.clone()),
            )
            .route(
                "/api/v1/gc/runs/{run_id}/candidates",
                Requires::Admin,
                get(gc_candidates).with_state(state.clone()),
            )
            .route(
                "/api/v1/gc/runs/{run_id}/actions/stop",
                Requires::Admin,
                post(stop_gc).with_state(state.clone()),
            )
            .route(
                "/api/v1/tasks",
                Requires::Admin,
                get(list_tasks).with_state(state.clone()),
            )
            .route(
                "/api/v1/tasks/{task_id}",
                Requires::Admin,
                get(get_task).with_state(state.clone()),
            )
            .route(
                "/api/v1/tasks/{task_id}/runs",
                Requires::Admin,
                get(list_task_runs).with_state(state.clone()),
            )
            .route(
                "/api/v1/tasks/{task_id}/runs/{run_id}",
                Requires::Admin,
                get(get_task_run).with_state(state.clone()),
            )
            .route(
                "/api/v1/tasks/{task_id}/runs/{run_id}/log",
                Requires::Admin,
                get(task_log).with_state(state),
            )
    }
}

#[derive(Clone)]
struct OpsApi {
    cleanup: Arc<PgCleanupPolicyRepo>,
    runs: Arc<PgLifecycleRunRepo>,
    gc: Arc<PgGcRunRepo>,
    repositories: Arc<dyn RepositoryRepo>,
    clock: Arc<dyn Clock>,
    pool: sqlx::PgPool,
}

#[derive(Debug, Deserialize)]
struct PageQuery {
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    dry_run: Option<bool>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    policy_id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    system: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    from_line: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PolicyPath {
    policy_id: String,
}

#[derive(Debug, Deserialize)]
struct RunPath {
    run_id: String,
}

#[derive(Debug, Deserialize)]
struct TaskPath {
    task_id: String,
}

#[derive(Debug, Deserialize)]
struct TaskRunPath {
    task_id: String,
    run_id: String,
}

#[derive(Debug, Deserialize)]
struct PolicyBody {
    name: String,
    #[serde(default)]
    description: String,
    format: String,
    criteria: Value,
}

fn page_request(query: &PageQuery) -> PageRequest {
    PageRequest {
        limit: query.limit.unwrap_or(50).clamp(1, 200),
        after: query.after.clone(),
    }
}

fn stamp(value: Option<OffsetDateTime>, fallback: OffsetDateTime) -> String {
    value
        .unwrap_or(fallback)
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn opt_stamp(value: Option<OffsetDateTime>) -> Value {
    value
        .and_then(|at| at.format(&Rfc3339).ok())
        .map_or(Value::Null, Value::String)
}

fn etag(version: i64) -> String {
    format!("\"{version}\"")
}

fn if_match(headers: &HeaderMap) -> Result<i64, ApiError> {
    let raw = headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .trim()
        .trim_matches('"');
    raw.parse::<i64>().map_err(|_| {
        ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed",
            "The resource changed. Reload it and try again",
        )
    })
}

fn with_etag(body: Value, version: i64) -> Response {
    let mut response = Json(body).into_response();
    if let Ok(value) = http::HeaderValue::from_str(&etag(version)) {
        response.headers_mut().insert(ETAG, value);
    }
    response
}

fn parse_uuid(raw: &str) -> Result<Uuid, ApiError> {
    raw.parse().map_err(|_| ApiError::route_not_found())
}

fn opt_u32(value: &Value, key: &str) -> Result<Option<u32>, ApiError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| {
                ApiError::bad_request("invalid_input", format!("{key} is out of range"))
            }),
        Some(_) => Err(ApiError::bad_request(
            "invalid_input",
            format!("{key} must be a number"),
        )),
    }
}

fn criteria_from_api(value: &Value) -> Result<CleanupCriteria, ApiError> {
    if !value.is_object() {
        return Err(ApiError::bad_request(
            "invalid_input",
            "criteria must be an object",
        ));
    }
    let name_regex = match value.get("name_pattern") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            return Err(ApiError::bad_request(
                "invalid_input",
                "name_pattern must be a string",
            ));
        }
    };
    let release_type = match value.get("release_type") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text == "release" => Some(ReleaseType::Release),
        Some(Value::String(text)) if text == "prerelease" => Some(ReleaseType::Prerelease),
        Some(_) => {
            return Err(ApiError::bad_request(
                "invalid_input",
                "release_type must be release or prerelease",
            ));
        }
    };
    let criteria = CleanupCriteria {
        max_age_days: opt_u32(value, "older_than_days")?,
        not_downloaded_days: opt_u32(value, "last_downloaded_days")?,
        keep_latest_versions: opt_u32(value, "keep_last_versions")?,
        name_regex,
        release_type,
        ..CleanupCriteria::default()
    };
    criteria.validate().map_err(|_| {
        ApiError::bad_request("invalid_input", "A cleanup policy needs one criterion")
    })?;
    Ok(criteria)
}

fn criteria_to_api(criteria: &CleanupCriteria) -> Value {
    json!({
        "older_than_days": criteria.max_age_days,
        "last_downloaded_days": criteria.not_downloaded_days,
        "keep_last_versions": criteria.keep_latest_versions,
        "name_pattern": criteria.name_regex,
        "release_type": criteria.release_type.map(|kind| match kind {
            ReleaseType::Release => "release",
            ReleaseType::Prerelease => "prerelease",
        }),
    })
}

fn status_name(status: TaskRunStatus) -> &'static str {
    match status {
        TaskRunStatus::Queued => "queued",
        TaskRunStatus::Running => "running",
        TaskRunStatus::Succeeded => "succeeded",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Skipped => "skipped",
        TaskRunStatus::Abandoned => "abandoned",
        _ => "failed",
    }
}

fn gc_status(run: &GcRun) -> &'static str {
    match run.status {
        TaskRunStatus::Cancelled if run.stop_requested => "stopped",
        TaskRunStatus::Cancelled | TaskRunStatus::Skipped | TaskRunStatus::Abandoned => "failed",
        other => status_name(other),
    }
}

async fn policy_view(api: &OpsApi, policy: &CleanupPolicy) -> Result<Value, ApiError> {
    let using = api.cleanup.repositories_using(policy.id).await?;
    let repos = api.repositories.list().await?;
    let attached = repos
        .into_iter()
        .filter(|repo| using.contains(&repo.id))
        .map(|repo| {
            json!({
                "id": repo.id.as_uuid(),
                "name": repo.name.as_str(),
                "format": repo.format,
                "repo_type": repo.repo_type,
            })
        })
        .collect::<Vec<_>>();
    let last = api
        .runs
        .list(
            Some(LifecycleKind::Cleanup),
            Some(PolicyRef::Cleanup(policy.id, None)),
            &PageRequest {
                limit: 1,
                after: None,
            },
        )
        .await?
        .items
        .into_iter()
        .next();
    let last_run = last.map(|run| {
        json!({
            "id": run.id.as_uuid(),
            "dry_run": run.dry_run,
            "status": status_name(run.status),
            "finished_at": opt_stamp(run.finished_at),
            "candidates": run.counts.selected,
            "bytes": run.counts.bytes_selected,
        })
    });
    Ok(json!({
        "id": policy.id.as_uuid(),
        "name": policy.name,
        "description": policy.description,
        "format": policy.format,
        "criteria": criteria_to_api(&policy.criteria),
        "created_at": stamp(Some(policy.created_at), policy.created_at),
        "updated_at": stamp(Some(policy.updated_at), policy.updated_at),
        "attached_repositories": attached,
        "last_run": last_run,
    }))
}

fn build_policy(
    body: PolicyBody,
    id: CleanupPolicyId,
    now: OffsetDateTime,
) -> Result<CleanupPolicy, ApiError> {
    let format = serde_json::from_value::<RepoFormat>(Value::String(body.format))
        .map_err(|_| ApiError::bad_request("invalid_input", "Unknown format"))?;
    let mut policy = CleanupPolicy::new(id, body.name, criteria_from_api(&body.criteria)?, now);
    policy.description = body.description;
    policy.format = Some(format);
    policy
        .validate()
        .map_err(|_| ApiError::bad_request("invalid_input", "The cleanup policy is not valid"))?;
    Ok(policy)
}

async fn list_policies(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut items = Vec::new();
    for policy in api.cleanup.list().await? {
        if query.format.as_ref().is_some_and(|format| {
            policy.format.map(rustryx_core::model::RepoFormat::as_str) != Some(format.as_str())
        }) {
            continue;
        }
        if query.q.as_ref().is_some_and(|needle| {
            !policy
                .name
                .to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase())
        }) {
            continue;
        }
        items.push(policy_view(&api, &policy).await?);
    }
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn get_policy(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<PolicyPath>,
) -> Result<Response, ApiError> {
    let id = CleanupPolicyId::from_uuid(parse_uuid(&path.policy_id)?);
    let policy = api
        .cleanup
        .get(id)
        .await?
        .ok_or_else(ApiError::route_not_found)?;
    Ok(with_etag(policy_view(&api, &policy).await?, policy.version))
}

async fn create_policy(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiJson(body): ApiJson<PolicyBody>,
) -> Result<Response, ApiError> {
    let policy = build_policy(body, CleanupPolicyId::generate(), api.clock.now())?;
    api.cleanup.create(&policy).await.map_err(conflict)?;
    let mut response =
        (StatusCode::CREATED, Json(policy_view(&api, &policy).await?)).into_response();
    if let Ok(value) = http::HeaderValue::from_str(&etag(policy.version)) {
        response.headers_mut().insert(ETAG, value);
    }
    Ok(response)
}

async fn update_policy(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    headers: HeaderMap,
    ApiPath(path): ApiPath<PolicyPath>,
    ApiJson(body): ApiJson<PolicyBody>,
) -> Result<Response, ApiError> {
    let id = CleanupPolicyId::from_uuid(parse_uuid(&path.policy_id)?);
    let current = api
        .cleanup
        .get(id)
        .await?
        .ok_or_else(ApiError::route_not_found)?;
    let expected = if_match(&headers)?;
    if expected != current.version {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed",
            "The resource changed. Reload it and try again",
        ));
    }
    let mut policy = build_policy(body, id, current.created_at)?;
    policy.version = current.version;
    policy.created_at = current.created_at;
    api.cleanup
        .update(&policy, expected)
        .await
        .map_err(conflict)?;
    let stored = api
        .cleanup
        .get(id)
        .await?
        .ok_or_else(ApiError::route_not_found)?;
    Ok(with_etag(policy_view(&api, &stored).await?, stored.version))
}

async fn delete_policy(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<PolicyPath>,
    ApiQuery(query): ApiQuery<ForceQuery>,
) -> Result<StatusCode, ApiError> {
    let id = CleanupPolicyId::from_uuid(parse_uuid(&path.policy_id)?);
    if query.force.unwrap_or(false) {
        sqlx::query("UPDATE repositories SET cleanup_policy_ids = array_remove(cleanup_policy_ids, $1) WHERE $1 = ANY(cleanup_policy_ids)")
            .bind(id.as_uuid())
            .execute(&api.pool)
            .await
            .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "The policy store is unavailable"))?;
    }
    api.cleanup.delete(id).await.map_err(conflict)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct ForceQuery {
    #[serde(default)]
    force: Option<bool>,
}

fn conflict(error: rustryx_core::error::StoreError) -> ApiError {
    match error {
        rustryx_core::error::StoreError::Conflict(message) if message.contains("already") => {
            ApiError::new(StatusCode::CONFLICT, "already_exists", message)
        }
        rustryx_core::error::StoreError::Conflict(message) => {
            ApiError::new(StatusCode::CONFLICT, "in_use", message)
        }
        other => other.into(),
    }
}

async fn preview_policy(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<PolicyPath>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = CleanupPolicyId::from_uuid(parse_uuid(&path.policy_id)?);
    let policy = api
        .cleanup
        .get(id)
        .await?
        .ok_or_else(ApiError::route_not_found)?;
    let now = api.clock.now();
    let mut run = LifecycleRun::new(
        LifecycleRunId::generate(),
        LifecycleKind::Cleanup,
        true,
        policy.version,
    );
    run.cleanup_policy_id = Some(policy.id);
    run.status = TaskRunStatus::Succeeded;
    run.started_at = Some(now);
    run.finished_at = Some(now);
    api.runs.create(&run).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(lifecycle_view(&run, Some(&policy.name), now)),
    ))
}

fn lifecycle_view(run: &LifecycleRun, policy_name: Option<&str>, now: OffsetDateTime) -> Value {
    let kind = match run.kind {
        LifecycleKind::Retention | LifecycleKind::LabelCleanup => "retention",
        LifecycleKind::Cleanup => "cleanup",
    };
    json!({
        "id": run.id.as_uuid(),
        "kind": kind,
        "policy_id": run.cleanup_policy_id.map(|id| id.as_uuid().to_string()),
        "policy_name": policy_name,
        "dry_run": run.dry_run,
        "status": status_name(run.status),
        "trigger": if run.requested_by.is_some() { "manual" } else { "api" },
        "triggered_by": Value::Null,
        "task_run_id": run.task_run_id.map(|id| id.as_uuid().to_string()),
        "queued_at": stamp(run.started_at.or(run.finished_at), now),
        "started_at": opt_stamp(run.started_at),
        "finished_at": opt_stamp(run.finished_at),
        "summary": {
            "candidates": run.counts.selected,
            "bytes": run.counts.bytes_selected,
            "repositories": 0,
            "protected": run.counts.protected_immutable + run.counts.protected_hold + run.counts.protected_last_version,
        },
        "error_code": Value::Null,
        "report_expires_at": Value::Null,
    })
}

async fn names_for(api: &OpsApi, runs: &[LifecycleRun]) -> Result<Vec<Option<String>>, ApiError> {
    let policies = api.cleanup.list().await?;
    Ok(runs
        .iter()
        .map(|run| {
            run.cleanup_policy_id.and_then(|id| {
                policies
                    .iter()
                    .find(|policy| policy.id == id)
                    .map(|policy| policy.name.clone())
            })
        })
        .collect())
}

async fn list_lifecycle(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let page = api
        .runs
        .list(Some(LifecycleKind::Cleanup), None, &page_request(&query))
        .await?;
    let names = names_for(&api, &page.items).await?;
    let now = api.clock.now();
    let items = page
        .items
        .iter()
        .zip(names)
        .filter(|(run, _)| query.dry_run.is_none_or(|dry| run.dry_run == dry))
        .filter(|(run, _)| {
            query
                .status
                .as_ref()
                .is_none_or(|status| status_name(run.status) == status)
        })
        .filter(|(run, _)| {
            query.policy_id.as_ref().is_none_or(|id| {
                run.cleanup_policy_id
                    .is_some_and(|policy| policy.as_uuid().to_string() == *id)
            })
        })
        .map(|(run, name)| lifecycle_view(run, name.as_deref(), now))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": page.next })))
}

async fn get_lifecycle(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<RunPath>,
) -> Result<Json<Value>, ApiError> {
    let id = LifecycleRunId::from_uuid(parse_uuid(&path.run_id)?);
    let run = api
        .runs
        .get(id)
        .await?
        .ok_or_else(ApiError::route_not_found)?;
    let name = names_for(&api, std::slice::from_ref(&run))
        .await?
        .into_iter()
        .next()
        .flatten();
    Ok(Json(lifecycle_view(&run, name.as_deref(), api.clock.now())))
}

fn candidate_view(item: &LifecycleItem) -> Value {
    let protected = match item.decision {
        ItemDecision::ProtectedImmutable => Some("immutability"),
        ItemDecision::ProtectedHold => Some("hold"),
        ItemDecision::ProtectedLastVersion => Some("last_version"),
        ItemDecision::Delete | ItemDecision::DeleteFailed => None,
    };
    json!({
        "repository": item.repository,
        "component_id": item.component_id.as_uuid(),
        "asset_id": Value::Null,
        "coordinate": item.coordinate,
        "size_bytes": item.size_bytes,
        "age_days": 0,
        "last_downloaded_at": Value::Null,
        "reason": if protected.is_some() { "older_than_days" } else { reason_or_default(&item.reason) },
        "protected_by": protected,
    })
}

fn reason_or_default(reason: &str) -> &str {
    match reason {
        "older_than_days"
        | "last_downloaded_days"
        | "keep_last_versions"
        | "name_pattern"
        | "release_type" => reason,
        _ => "older_than_days",
    }
}

async fn lifecycle_candidates(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<RunPath>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = LifecycleRunId::from_uuid(parse_uuid(&path.run_id)?);
    if api.runs.get(id).await?.is_none() {
        return Err(ApiError::route_not_found());
    }
    let page = api.runs.list_items(id, &page_request(&query)).await?;
    let items = page.items.iter().map(candidate_view).collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": page.next })))
}

fn gc_view(run: &GcRun, now: OffsetDateTime) -> Value {
    let per_store = if run.per_store.is_array() {
        run.per_store.clone()
    } else {
        json!([])
    };
    json!({
        "id": run.id.as_uuid(),
        "trigger": if run.requested_by.is_some() { "manual" } else { "schedule" },
        "dry_run": run.options.dry_run,
        "delete_untagged": run.options.delete_untagged,
        "workers": run.options.workers,
        "status": gc_status(run),
        "queued_at": stamp(run.started_at.or(run.finished_at), now),
        "started_at": opt_stamp(run.started_at),
        "finished_at": opt_stamp(run.finished_at),
        "candidate_blobs": run.counts.candidates,
        "reclaimed_bytes": if run.options.dry_run { run.counts.bytes_candidates } else { run.counts.bytes_deleted },
        "per_blob_store": per_store,
        "task_run_id": run.task_run_id.map(|id| id.as_uuid().to_string()),
        "log_available": run.task_run_id.is_some(),
    })
}

async fn list_gc(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let page = api.gc.list(&page_request(&query)).await?;
    let now = api.clock.now();
    let items = page
        .items
        .iter()
        .filter(|run| query.dry_run.is_none_or(|dry| run.options.dry_run == dry))
        .filter(|run| {
            query
                .status
                .as_ref()
                .is_none_or(|status| gc_status(run) == status)
        })
        .map(|run| gc_view(run, now))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": page.next })))
}

async fn get_gc(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<RunPath>,
) -> Result<Json<Value>, ApiError> {
    let id = GcRunId::from_uuid(parse_uuid(&path.run_id)?);
    let run = api
        .gc
        .get(id)
        .await?
        .ok_or_else(ApiError::route_not_found)?;
    Ok(Json(gc_view(&run, api.clock.now())))
}

async fn gc_candidates(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<RunPath>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = GcRunId::from_uuid(parse_uuid(&path.run_id)?);
    if api.gc.get(id).await?.is_none() {
        return Err(ApiError::route_not_found());
    }
    let page = api.gc.list_samples(id, &page_request(&query)).await?;
    let items = page
        .items
        .iter()
        .map(|sample| {
            json!({
                "blob_store": sample.blob_store,
                "key": sample.key,
                "size_bytes": sample.size_bytes,
                "orphaned_at": stamp(Some(sample.orphaned_at), sample.orphaned_at),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": page.next })))
}

async fn stop_gc(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<RunPath>,
) -> Result<Json<Value>, ApiError> {
    let id = GcRunId::from_uuid(parse_uuid(&path.run_id)?);
    if !api.gc.request_stop(id).await? {
        return Err(ApiError::route_not_found());
    }
    let run = api
        .gc
        .get(id)
        .await?
        .ok_or_else(ApiError::route_not_found)?;
    Ok(Json(gc_view(&run, api.clock.now())))
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "The task store is unavailable",
    )
}

fn schedule_view(raw: &Value) -> Value {
    let kind = raw.get("type").and_then(Value::as_str).unwrap_or("manual");
    let kind = match kind {
        "manual" | "hourly" | "daily" | "weekly" | "monthly" | "cron" | "once" => kind,
        "every" => "hourly",
        _ => "manual",
    };
    json!({
        "type": kind,
        "cron": raw.get("expr").cloned().unwrap_or(Value::Null),
        "at": raw.get("at").cloned().unwrap_or(Value::Null),
        "next_runs": [],
    })
}

fn task_run_view(row: &sqlx::postgres::PgRow) -> Value {
    let summary: Value = row.try_get("summary").unwrap_or(Value::Null);
    let error: Option<String> = row.try_get("error").unwrap_or(None);
    json!({
        "id": row.try_get::<Uuid, _>("id").unwrap_or(Uuid::nil()),
        "task_id": row.try_get::<Option<Uuid>, _>("task_id").unwrap_or(None),
        "status": row.try_get::<String, _>("status").unwrap_or_else(|_| "failed".into()),
        "dry_run": row.try_get::<bool, _>("dry_run").unwrap_or(false),
        "trigger": row.try_get::<String, _>("trigger").unwrap_or_else(|_| "manual".into()),
        "triggered_by": Value::Null,
        "node": row.try_get::<Option<String>, _>("node_id").unwrap_or(None),
        "queued_at": opt_stamp(row.try_get("queued_at").ok()),
        "started_at": opt_stamp(row.try_get("started_at").ok()),
        "finished_at": opt_stamp(row.try_get("finished_at").ok()),
        "duration_ms": Value::Null,
        "summary": summary,
        "error_code": error,
        "log_available": true,
        "log_expires_at": Value::Null,
    })
}

async fn load_task(
    pool: &sqlx::PgPool,
    id: Uuid,
) -> Result<Option<sqlx::postgres::PgRow>, ApiError> {
    sqlx::query(
        "SELECT id, kind, name, params, schedule, enabled, system, next_run_at, last_run_id \
         FROM task_definitions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|_| unavailable())
}

async fn latest_run(pool: &sqlx::PgPool, task: Uuid) -> Result<Option<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT id, task_id, status, dry_run, trigger, node_id, queued_at, started_at, finished_at, summary, error \
         FROM task_runs WHERE task_id = $1 ORDER BY queued_at DESC LIMIT 1",
    )
    .bind(task)
    .fetch_optional(pool)
    .await
    .map_err(|_| unavailable())?;
    Ok(row.as_ref().map(task_run_view))
}

fn task_view(row: &sqlx::postgres::PgRow, last_run: Option<Value>) -> Value {
    let kind: String = row.try_get("kind").unwrap_or_default();
    let schedule: Value = row.try_get("schedule").unwrap_or(json!({}));
    let params: Value = row.try_get("params").unwrap_or(json!({}));
    json!({
        "id": row.try_get::<Uuid, _>("id").unwrap_or(Uuid::nil()),
        "name": row.try_get::<String, _>("name").unwrap_or_default(),
        "kind": kind.replace('_', "-"),
        "description": kind.replace('_', " "),
        "system": row.try_get::<bool, _>("system").unwrap_or(true),
        "enabled": row.try_get::<bool, _>("enabled").unwrap_or(false),
        "schedule": schedule_view(&schedule),
        "parameters": params,
        "last_run": last_run,
        "next_run_at": opt_stamp(row.try_get("next_run_at").ok()),
        "lock": { "held_by": Value::Null },
    })
}

async fn list_tasks(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, kind, name, params, schedule, enabled, system, next_run_at, last_run_id \
         FROM task_definitions ORDER BY name",
    )
    .fetch_all(&api.pool)
    .await
    .map_err(|_| unavailable())?;
    let mut items = Vec::new();
    for row in rows {
        let kind: String = row.try_get("kind").unwrap_or_default();
        let system: bool = row.try_get("system").unwrap_or(false);
        let enabled: bool = row.try_get("enabled").unwrap_or(false);
        if query
            .kind
            .as_ref()
            .is_some_and(|want| want != &kind.replace('_', "-") && want != &kind)
            || query.system.is_some_and(|want| want != system)
            || query.enabled.is_some_and(|want| want != enabled)
        {
            continue;
        }
        let id: Uuid = row.try_get("id").unwrap_or(Uuid::nil());
        items.push(task_view(&row, latest_run(&api.pool, id).await?));
    }
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn get_task(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<TaskPath>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_uuid(&path.task_id)?;
    let row = load_task(&api.pool, id)
        .await?
        .ok_or_else(ApiError::route_not_found)?;
    Ok(Json(task_view(&row, latest_run(&api.pool, id).await?)))
}

async fn list_task_runs(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<TaskPath>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_uuid(&path.task_id)?;
    if load_task(&api.pool, id).await?.is_none() {
        return Err(ApiError::route_not_found());
    }
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 200));
    let rows = sqlx::query(
        "SELECT id, task_id, status, dry_run, trigger, node_id, queued_at, started_at, finished_at, summary, error \
         FROM task_runs WHERE task_id = $1 ORDER BY queued_at DESC LIMIT $2",
    )
    .bind(id)
    .bind(limit)
    .fetch_all(&api.pool)
    .await
    .map_err(|_| unavailable())?;
    let items = rows
        .iter()
        .map(task_run_view)
        .filter(|run| {
            query
                .status
                .as_ref()
                .is_none_or(|status| run.get("status").and_then(Value::as_str) == Some(status))
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn get_task_run(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<TaskRunPath>,
) -> Result<Json<Value>, ApiError> {
    let task = parse_uuid(&path.task_id)?;
    let run = parse_uuid(&path.run_id)?;
    let row = sqlx::query(
        "SELECT id, task_id, status, dry_run, trigger, node_id, queued_at, started_at, finished_at, summary, error \
         FROM task_runs WHERE id = $1 AND task_id = $2",
    )
    .bind(run)
    .bind(task)
    .fetch_optional(&api.pool)
    .await
    .map_err(|_| unavailable())?
    .ok_or_else(ApiError::route_not_found)?;
    Ok(Json(task_run_view(&row)))
}

fn log_level(level: i16) -> &'static str {
    match level {
        i16::MIN..=1 => "debug",
        2 => "info",
        3 => "warn",
        _ => "error",
    }
}

async fn task_log(
    State(api): State<OpsApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<TaskRunPath>,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let task = parse_uuid(&path.task_id)?;
    let run = parse_uuid(&path.run_id)?;
    let exists = sqlx::query("SELECT 1 FROM task_runs WHERE id = $1 AND task_id = $2")
        .bind(run)
        .bind(task)
        .fetch_optional(&api.pool)
        .await
        .map_err(|_| unavailable())?;
    if exists.is_none() {
        return Err(ApiError::route_not_found());
    }
    let from = i32::try_from(query.from_line.unwrap_or(0).max(0)).unwrap_or(i32::MAX);
    let limit = i64::from(query.limit.unwrap_or(500).clamp(1, 500));
    let rows = sqlx::query(
        "SELECT seq, at, level, message FROM task_run_log WHERE run_id = $1 AND seq >= $2 ORDER BY seq LIMIT $3",
    )
    .bind(run)
    .bind(from)
    .bind(limit)
    .fetch_all(&api.pool)
    .await
    .map_err(|_| unavailable())?;
    let lines = rows
        .iter()
        .map(|row| {
            let level: i16 = row.try_get("level").unwrap_or(2);
            json!({
                "n": row.try_get::<i32, _>("seq").unwrap_or(0),
                "at": opt_stamp(row.try_get("at").ok()),
                "level": log_level(level),
                "message": row.try_get::<String, _>("message").unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let next = lines
        .last()
        .and_then(|line| line.get("n"))
        .and_then(Value::as_i64)
        .map(|n| n + 1);
    Ok(Json(json!({
        "run_id": run,
        "lines": lines,
        "next_line": next,
        "finished": true,
        "purged": false,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_criteria_round_trip_through_the_domain_names() {
        let criteria = criteria_from_api(&json!({
            "older_than_days": 90,
            "last_downloaded_days": null,
            "keep_last_versions": 5,
            "name_pattern": "SNAPSHOT",
            "release_type": "prerelease"
        }))
        .expect("criteria");
        assert_eq!(criteria.max_age_days, Some(90));
        assert_eq!(criteria.keep_latest_versions, Some(5));
        assert_eq!(criteria.name_regex.as_deref(), Some("SNAPSHOT"));
        assert_eq!(criteria.release_type, Some(ReleaseType::Prerelease));
        let back = criteria_to_api(&criteria);
        assert_eq!(back["older_than_days"], 90);
        assert_eq!(back["name_pattern"], "SNAPSHOT");
        assert!(criteria_from_api(&json!({})).is_err());
    }
}
