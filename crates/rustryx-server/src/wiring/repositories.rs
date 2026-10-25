//! `/api/v1/repositories` and `GET /api/v1/blob-stores`.
//!
//! The list omits a repository the caller may not browse. Creating one needs
//! `nx-admin`. A taken name is `409 already_exists`. An unknown blob store, a
//! missing member or a member of another format is `400`, never `404`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::header::{ETAG, HOST, LOCATION};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http::HeaderMap;
use rustryx_api::{
    AdminGrant, ApiError, ApiJson, ApiPath, ApiQuery, Authorized, RequestPrincipal, Requires,
    RouteModule, RouteSet,
};
use rustryx_core::error::StoreError;
use rustryx_core::identity::{Action, Principal};
use rustryx_core::ids::{BlobStoreId, RepositoryId};
use rustryx_core::model::{
    BlobStoreDef, BlobStoreKind, GroupConfig, ProxyConfig, RepoFormat, RepoType, Repository,
    WritePolicy,
};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{
    BlobStoreRepo, Clock, Decision, RbacChecker, RepositoryRepo, ResourceRef, ScanRepo,
};
use rustryx_core::scan::{LicenseBuckets, RepositoryHealth};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use url::Url;

/// Public URL and upstream rules the repository routes need.
pub struct RepoHttp {
    /// `server.base_url`.
    pub base_url: Option<Url>,
    /// When no base URL is set, client URLs use this scheme with the request host.
    pub require_https: bool,
    /// Global anonymous switch. A repository URL is anonymously readable only when this is on too.
    pub anonymous_enabled: bool,
    /// Same SSRF policy the format engine uses for upstream fetches.
    pub upstream: UpstreamUrlPolicy,
}

/// Routes for repositories and the blob store picker the create form needs.
pub struct RepositoriesModule {
    state: RepoApi,
}

impl RepositoriesModule {
    /// `base_url` is `server.base_url`. Without it, client URLs use the request host.
    #[must_use]
    pub fn new(
        repositories: Arc<dyn RepositoryRepo>,
        blob_stores: Arc<dyn BlobStoreRepo>,
        rbac: Arc<dyn RbacChecker>,
        clock: Arc<dyn Clock>,
        http: RepoHttp,
    ) -> Self {
        Self {
            state: RepoApi {
                repositories,
                blob_stores,
                rbac,
                clock,
                base_url: http.base_url,
                fallback_scheme: if http.require_https { "https" } else { "http" },
                anonymous_enabled: http.anonymous_enabled,
                upstream: http.upstream,
                scans: None,
            },
        }
    }

    /// Counts repository health from stored findings. Without this, `include=stats` still
    /// returns zeros and `scanned: false`.
    #[must_use]
    #[allow(
        dead_code,
        reason = "the PostgreSQL store does not implement ScanRepo yet"
    )]
    pub fn with_scans(mut self, scans: Arc<dyn ScanRepo>) -> Self {
        self.state.scans = Some(scans);
        self
    }
}

impl RouteModule for RepositoriesModule {
    fn name(&self) -> &'static str {
        "repositories"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let state = self.state.clone();
        routes
            .route(
                "/api/v1/repositories",
                Requires::Public,
                get(list).with_state(state.clone()),
            )
            .route(
                "/api/v1/repositories",
                Requires::Admin,
                post(create).with_state(state.clone()),
            )
            .route(
                "/api/v1/repositories/{name}",
                Requires::Public,
                get(get_one).with_state(state.clone()),
            )
            .route(
                "/api/v1/blob-stores",
                Requires::Admin,
                get(list_stores).with_state(state),
            )
    }
}

#[derive(Clone)]
struct RepoApi {
    repositories: Arc<dyn RepositoryRepo>,
    blob_stores: Arc<dyn BlobStoreRepo>,
    rbac: Arc<dyn RbacChecker>,
    clock: Arc<dyn Clock>,
    base_url: Option<Url>,
    fallback_scheme: &'static str,
    anonymous_enabled: bool,
    upstream: UpstreamUrlPolicy,
    scans: Option<Arc<dyn ScanRepo>>,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    format: Option<RepoFormat>,
    #[serde(default)]
    repo_type: Option<RepoType>,
    #[serde(default)]
    online: Option<bool>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    include: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    name: RepoName,
    format: RepoFormat,
    repo_type: RepoType,
    blob_store_id: BlobStoreId,
    #[serde(default)]
    write_policy: Option<WritePolicy>,
    #[serde(default)]
    allow_anonymous: bool,
    #[serde(default = "yes")]
    online: bool,
    #[serde(default)]
    quota_bytes: Option<u64>,
    #[serde(default)]
    format_config: serde_json::Value,
    #[serde(default)]
    proxy: Option<ProxyBody>,
    #[serde(default)]
    group: Option<GroupBody>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct ProxyBody {
    remote_url: String,
    #[serde(default)]
    content_max_age_secs: Option<u64>,
    #[serde(default)]
    metadata_max_age_secs: Option<u64>,
    #[serde(default)]
    negative_cache_ttl_secs: Option<u64>,
    #[serde(default)]
    max_concurrent_upstream: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GroupBody {
    members: Vec<RepoName>,
}

#[derive(Serialize)]
struct Page<T> {
    items: Vec<T>,
    next: Option<String>,
}

#[derive(Serialize)]
struct RepositoryView {
    id: RepositoryId,
    name: RepoName,
    format: RepoFormat,
    repo_type: RepoType,
    blob_store_id: BlobStoreId,
    write_policy: WritePolicy,
    allow_anonymous: bool,
    online: bool,
    quota_bytes: Option<u64>,
    cleanup_policy_ids: Vec<rustryx_core::ids::CleanupPolicyId>,
    routing_rule_id: Option<rustryx_core::ids::RoutingRuleId>,
    proxy: Option<ProxyView>,
    group: Option<GroupConfig>,
    format_config: serde_json::Value,
    #[serde(with = "rustryx_core::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "rustryx_core::rfc3339")]
    updated_at: OffsetDateTime,
    blob_store: StoreRef,
    url: String,
    cleanup_policies: Vec<Named>,
    routing_rule: Option<Named>,
    stats: Option<Stats>,
    /// Present when the caller asked for `include=stats`.
    health: Option<HealthView>,
    permissions: Permissions,
    upstream: Option<Quiet>,
    anonymous_effective: bool,
}

#[derive(Serialize)]
struct ProxyView {
    remote_url: String,
    auth: Option<RedactedAuth>,
    content_max_age_secs: u64,
    metadata_max_age_secs: u64,
    negative_cache_ttl_secs: u64,
    max_concurrent_upstream: u32,
    registry_id: Option<rustryx_core::ids::RegistryId>,
    ca_certificate_ids: Vec<rustryx_core::ids::TrustedCertificateId>,
    namespace_allowlist: Vec<String>,
    bandwidth_limit_kbps: Option<u32>,
}

#[derive(Serialize)]
struct RedactedAuth {
    username: String,
    password_set: bool,
}

#[derive(Serialize)]
struct StoreRef {
    id: BlobStoreId,
    name: String,
    kind: BlobStoreKind,
}

#[derive(Serialize)]
struct Named {
    id: String,
    name: String,
}

#[derive(Serialize)]
struct Stats {
    component_count: u64,
    asset_count: u64,
    size_bytes: u64,
    #[serde(with = "rustryx_core::rfc3339")]
    as_of: OffsetDateTime,
}

#[derive(Serialize)]
struct HealthView {
    scanned: bool,
    component_count: u64,
    scanned_components: u64,
    critical: u64,
    severe: u64,
    moderate: u64,
    licenses: LicenseBuckets,
    #[serde(with = "rustryx_core::rfc3339::option")]
    scanned_at: Option<OffsetDateTime>,
}

fn health_view(health: &RepositoryHealth) -> HealthView {
    HealthView {
        scanned: health.scanned,
        component_count: health.component_count,
        scanned_components: health.scanned_components,
        critical: health.critical,
        severe: health.severe,
        moderate: health.moderate,
        licenses: health.licenses,
        scanned_at: health.scanned_at,
    }
}

#[derive(Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the five flags are the API contract, not a state machine"
)]
struct Permissions {
    browse: bool,
    read: bool,
    write: bool,
    delete: bool,
    admin: bool,
}

/// Placeholder so `upstream` serializes as `null` until proxy health exists.
#[derive(Serialize)]
struct Quiet;

#[derive(Serialize)]
struct StoreView {
    id: BlobStoreId,
    name: String,
    kind: BlobStoreKind,
    config: serde_json::Value,
    quota_bytes: Option<u64>,
    #[serde(with = "rustryx_core::rfc3339")]
    created_at: OffsetDateTime,
    usage: Option<Stats>,
    health: Health,
    repository_count: u64,
    credentials: Option<RedactedAuth>,
}

#[derive(Serialize)]
struct Health {
    state: &'static str,
    #[serde(with = "rustryx_core::rfc3339")]
    checked_at: OffsetDateTime,
    last_error_code: Option<String>,
}

async fn list(
    State(api): State<RepoApi>,
    RequestPrincipal(principal): RequestPrincipal,
    ApiQuery(query): ApiQuery<ListQuery>,
    headers: HeaderMap,
) -> Result<Json<Page<RepositoryView>>, ApiError> {
    let (rows, next) = visible(&api, &principal, &query).await?;
    let origin = public_origin(&api, &headers);
    let with_stats = wants_stats(&query);
    let health = if with_stats {
        health_by_id(&api).await?
    } else {
        HashMap::new()
    };
    let mut items = Vec::with_capacity(rows.len());
    for repository in &rows {
        let row = health.get(&repository.id);
        items.push(view(&api, &principal, repository, &origin, with_stats, row).await?);
    }
    Ok(Json(Page { items, next }))
}

#[derive(Debug, Deserialize)]
struct NamePath {
    name: RepoName,
}

async fn get_one(
    State(api): State<RepoApi>,
    RequestPrincipal(principal): RequestPrincipal,
    ApiPath(path): ApiPath<NamePath>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let repository = load_visible(&api, &principal, &path.name).await?;
    let origin = public_origin(&api, &headers);
    let health = health_by_id(&api).await?;
    let body = view(
        &api,
        &principal,
        &repository,
        &origin,
        true,
        health.get(&repository.id),
    )
    .await?;
    let mut response = Json(body).into_response();
    let etag = format!("\"{}\"", repository.updated_at.unix_timestamp());
    if let Ok(value) = http::HeaderValue::from_str(&etag) {
        response.headers_mut().insert(ETAG, value);
    }
    Ok(response)
}

async fn create(
    State(api): State<RepoApi>,
    _proof: Authorized<AdminGrant>,
    RequestPrincipal(principal): RequestPrincipal,
    headers: HeaderMap,
    ApiJson(body): ApiJson<CreateBody>,
) -> Result<Response, ApiError> {
    let repository = build(&api, body).await?;
    match api.repositories.create(&repository).await {
        Ok(()) => {}
        Err(StoreError::Conflict(_)) => {
            return Err(ApiError::new(
                http::StatusCode::CONFLICT,
                "already_exists",
                "A repository with this name already exists",
            ));
        }
        Err(error) => return Err(error.into()),
    }
    let origin = public_origin(&api, &headers);
    let body = view(&api, &principal, &repository, &origin, false, None).await?;
    let mut response = (http::StatusCode::CREATED, Json(body)).into_response();
    let location = format!("/api/v1/repositories/{}", repository.name);
    if let Ok(value) = http::HeaderValue::from_str(&location) {
        response.headers_mut().insert(LOCATION, value);
    }
    Ok(response)
}

async fn list_stores(
    State(api): State<RepoApi>,
    _proof: Authorized<AdminGrant>,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<Page<StoreView>>, ApiError> {
    let repos = api.repositories.list().await?;
    let mut stores = api.blob_stores.list().await?;
    stores.retain(|store| store_matches(store, &query));
    stores.sort_by(|left, right| left.name.cmp(&right.name));
    let now = api.clock.now();
    let items = stores
        .iter()
        .map(|store| store_view(store, &repos, now))
        .collect();
    Ok(Json(Page { items, next: None }))
}

fn wants_stats(query: &ListQuery) -> bool {
    query
        .include
        .as_ref()
        .is_some_and(|value| value.split(',').any(|part| part.trim() == "stats"))
}

async fn visible(
    api: &RepoApi,
    principal: &Principal,
    query: &ListQuery,
) -> Result<(Vec<Repository>, Option<String>), ApiError> {
    let mut rows = api.repositories.list().await?;
    rows.retain(|repository| matches_query(repository, query));
    let mut kept = Vec::new();
    for repository in rows {
        if may_browse(api, principal, &repository).await? {
            kept.push(repository);
        }
    }
    sort_rows(&mut kept, query.sort.as_deref())?;
    page(kept, query)
}

fn matches_query(repository: &Repository, query: &ListQuery) -> bool {
    if query
        .format
        .is_some_and(|format| repository.format != format)
        || query
            .repo_type
            .is_some_and(|kind| repository.repo_type != kind)
        || query
            .online
            .is_some_and(|online| repository.online != online)
    {
        return false;
    }
    query.q.as_ref().is_none_or(|needle| {
        repository
            .name
            .as_str()
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    })
}

fn sort_rows(rows: &mut [Repository], sort: Option<&str>) -> Result<(), ApiError> {
    match sort.unwrap_or("name") {
        "name" => rows.sort_by(|left, right| left.name.cmp(&right.name)),
        "-name" => rows.sort_by(|left, right| right.name.cmp(&left.name)),
        "created_at" => rows.sort_by_key(|repository| repository.created_at),
        "-created_at" => rows.sort_by_key(|repository| std::cmp::Reverse(repository.created_at)),
        _ => {
            return Err(ApiError::bad_request(
                "invalid_query",
                "sort must be name, -name, created_at or -created_at",
            ));
        }
    }
    Ok(())
}

fn page(
    mut rows: Vec<Repository>,
    query: &ListQuery,
) -> Result<(Vec<Repository>, Option<String>), ApiError> {
    let limit = usize::try_from(query.limit.unwrap_or(50).clamp(1, 1000)).unwrap_or(usize::MAX);
    if let Some(after) = query.after.as_deref() {
        let Some(position) = rows
            .iter()
            .position(|repository| repository.name.as_str() == after)
        else {
            return Err(ApiError::bad_request(
                "invalid_page_token",
                "The page token is malformed or was issued for another request",
            ));
        };
        rows = rows.split_off(position.saturating_add(1));
    }
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = more
        .then(|| rows.last().map(|repository| repository.name.to_string()))
        .flatten();
    Ok((rows, next))
}

async fn load_visible(
    api: &RepoApi,
    principal: &Principal,
    name: &RepoName,
) -> Result<Repository, ApiError> {
    let Some(repository) = api.repositories.get_by_name(name).await? else {
        return Err(ApiError::route_not_found());
    };
    if may_browse(api, principal, &repository).await? {
        Ok(repository)
    } else {
        Err(ApiError::route_not_found())
    }
}

async fn build(api: &RepoApi, body: CreateBody) -> Result<Repository, ApiError> {
    if !body.format_config.is_object() && !body.format_config.is_null() {
        return Err(invalid("format_config must be an object"));
    }
    if api.blob_stores.get(body.blob_store_id).await?.is_none() {
        return Err(invalid("Unknown blob store"));
    }
    let mut repository = Repository::new(
        RepositoryId::generate(),
        body.name,
        body.format,
        body.repo_type,
        body.blob_store_id,
        api.clock.now(),
    );
    repository.allow_anonymous = body.allow_anonymous;
    repository.online = body.online;
    repository.quota_bytes = body.quota_bytes;
    if body.format_config.is_object() {
        repository.format_config = body.format_config;
    }
    match body.repo_type {
        RepoType::Hosted => {
            hosted(
                &mut repository,
                body.write_policy,
                body.proxy.is_some(),
                body.group.is_some(),
            )?;
        }
        RepoType::Proxy => {
            proxy_of(
                &mut repository,
                &api.upstream,
                body.proxy,
                body.group.is_some(),
            )?;
        }
        RepoType::Group => {
            group_of(
                &mut repository,
                &api.repositories,
                body.group,
                body.proxy.is_some(),
            )
            .await?;
        }
    }
    Ok(repository)
}

fn hosted(
    repository: &mut Repository,
    write_policy: Option<WritePolicy>,
    has_proxy: bool,
    has_group: bool,
) -> Result<(), ApiError> {
    if has_proxy || has_group {
        return Err(invalid(
            "A hosted repository has no proxy or group settings",
        ));
    }
    repository.write_policy = write_policy.unwrap_or(WritePolicy::AllowOnce);
    Ok(())
}

fn proxy_of(
    repository: &mut Repository,
    policy: &UpstreamUrlPolicy,
    proxy: Option<ProxyBody>,
    has_group: bool,
) -> Result<(), ApiError> {
    if has_group {
        return Err(invalid("A proxy repository has no group settings"));
    }
    let Some(proxy) = proxy else {
        return Err(invalid("A proxy repository needs an upstream URL"));
    };
    let url = Url::parse(&proxy.remote_url).map_err(|_| rejected())?;
    if policy.check_url(&url).is_err() {
        return Err(rejected());
    }
    let mut config = ProxyConfig::new(proxy.remote_url);
    if let Some(value) = proxy.content_max_age_secs {
        config.content_max_age_secs = value;
    }
    if let Some(value) = proxy.metadata_max_age_secs {
        config.metadata_max_age_secs = value;
    }
    if let Some(value) = proxy.negative_cache_ttl_secs {
        config.negative_cache_ttl_secs = value;
    }
    if let Some(value) = proxy.max_concurrent_upstream {
        config.max_concurrent_upstream = value;
    }
    repository.proxy = Some(config);
    repository.write_policy = WritePolicy::Deny;
    Ok(())
}

async fn group_of(
    repository: &mut Repository,
    repositories: &Arc<dyn RepositoryRepo>,
    group: Option<GroupBody>,
    has_proxy: bool,
) -> Result<(), ApiError> {
    if has_proxy {
        return Err(invalid("A group repository has no proxy settings"));
    }
    let Some(group) = group.filter(|group| !group.members.is_empty()) else {
        return Err(invalid("A group repository needs members"));
    };
    for member in &group.members {
        if member == &repository.name {
            return Err(invalid("A group cannot contain itself"));
        }
        let Some(existing) = repositories.get_by_name(member).await? else {
            return Err(invalid("A group member does not exist"));
        };
        if existing.format != repository.format {
            return Err(invalid("A group member must use the same format"));
        }
    }
    repository.group = Some(GroupConfig {
        members: group.members,
    });
    repository.write_policy = WritePolicy::Deny;
    Ok(())
}

async fn health_by_id(api: &RepoApi) -> Result<HashMap<RepositoryId, RepositoryHealth>, ApiError> {
    let Some(scans) = &api.scans else {
        return Ok(HashMap::new());
    };
    let repos = api.repositories.list().await?;
    let ids: Vec<RepositoryId> = repos.iter().map(|repository| repository.id).collect();
    let direct = scans.repository_health(&ids).await?;
    let by_id: HashMap<RepositoryId, RepositoryHealth> = direct
        .into_iter()
        .map(|health| (health.repository_id, health))
        .collect();
    let mut rolled = HashMap::with_capacity(repos.len());
    for repository in &repos {
        rolled.insert(
            repository.id,
            rollup(&repos, repository.id, &by_id, &mut HashSet::new()),
        );
    }
    Ok(rolled)
}

fn rollup(
    repos: &[Repository],
    id: RepositoryId,
    direct: &HashMap<RepositoryId, RepositoryHealth>,
    seen: &mut HashSet<RepositoryId>,
) -> RepositoryHealth {
    if !seen.insert(id) {
        return RepositoryHealth::unscanned(id);
    }
    let rolled = match repos.iter().find(|repository| repository.id == id) {
        Some(repository) if repository.repo_type == RepoType::Group => {
            let parts: Vec<RepositoryHealth> = repository
                .group
                .as_ref()
                .map(|group| &group.members)
                .into_iter()
                .flatten()
                .filter_map(|name| repos.iter().find(|member| &member.name == name))
                .map(|member| rollup(repos, member.id, direct, seen))
                .collect();
            RepositoryHealth::sum(id, &parts)
        }
        _ => direct
            .get(&id)
            .cloned()
            .unwrap_or_else(|| RepositoryHealth::unscanned(id)),
    };
    seen.remove(&id);
    rolled
}

async fn view(
    api: &RepoApi,
    principal: &Principal,
    repository: &Repository,
    origin: &str,
    with_stats: bool,
    health: Option<&RepositoryHealth>,
) -> Result<RepositoryView, ApiError> {
    let store = api
        .blob_stores
        .get(repository.blob_store_id)
        .await?
        .ok_or_else(|| ApiError::internal("repository blob store is missing"))?;
    let admin = admin_of(api, principal).await?;
    let permissions = permissions_of(api, principal, repository, admin).await?;
    Ok(RepositoryView {
        id: repository.id,
        name: repository.name.clone(),
        format: repository.format,
        repo_type: repository.repo_type,
        blob_store_id: repository.blob_store_id,
        write_policy: repository.write_policy,
        allow_anonymous: repository.allow_anonymous,
        online: repository.online,
        quota_bytes: repository.quota_bytes,
        cleanup_policy_ids: repository.cleanup_policy_ids.clone(),
        routing_rule_id: repository.routing_rule_id,
        proxy: repository.proxy.as_ref().map(proxy_view),
        group: repository.group.clone(),
        format_config: repository.format_config.clone(),
        created_at: repository.created_at,
        updated_at: repository.updated_at,
        blob_store: StoreRef {
            id: store.id,
            name: store.name,
            kind: store.kind,
        },
        url: format!("{origin}/repository/{}/", repository.name),
        cleanup_policies: Vec::new(),
        routing_rule: None,
        stats: with_stats.then(|| Stats {
            component_count: 0,
            asset_count: 0,
            size_bytes: 0,
            as_of: api.clock.now(),
        }),
        health: with_stats.then(|| {
            health_view(
                &health
                    .cloned()
                    .unwrap_or_else(|| RepositoryHealth::unscanned(repository.id)),
            )
        }),
        permissions,
        upstream: None,
        anonymous_effective: repository.allow_anonymous && api.anonymous_enabled,
    })
}

fn proxy_view(proxy: &ProxyConfig) -> ProxyView {
    ProxyView {
        remote_url: proxy.remote_url.clone(),
        auth: proxy.auth.as_ref().map(|auth| RedactedAuth {
            username: auth.username.clone(),
            password_set: true,
        }),
        content_max_age_secs: proxy.content_max_age_secs,
        metadata_max_age_secs: proxy.metadata_max_age_secs,
        negative_cache_ttl_secs: proxy.negative_cache_ttl_secs,
        max_concurrent_upstream: proxy.max_concurrent_upstream,
        registry_id: proxy.registry_id,
        ca_certificate_ids: proxy.ca_certificate_ids.clone(),
        namespace_allowlist: proxy.namespace_allowlist.clone(),
        bandwidth_limit_kbps: proxy.bandwidth_limit_kbps,
    }
}

fn store_view(store: &BlobStoreDef, repositories: &[Repository], now: OffsetDateTime) -> StoreView {
    let repository_count = repositories
        .iter()
        .filter(|repository| repository.blob_store_id == store.id)
        .count() as u64;
    StoreView {
        id: store.id,
        name: store.name.clone(),
        kind: store.kind,
        config: store.config.clone(),
        quota_bytes: store.quota_bytes,
        created_at: store.created_at,
        usage: None,
        health: Health {
            state: "ok",
            checked_at: now,
            last_error_code: None,
        },
        repository_count,
        credentials: None,
    }
}

fn store_matches(store: &BlobStoreDef, query: &ListQuery) -> bool {
    if query.q.as_ref().is_some_and(|prefix| {
        !store
            .name
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    }) {
        return false;
    }
    query
        .kind
        .as_ref()
        .is_none_or(|kind| kind_name(store.kind) == kind)
}

fn kind_name(kind: BlobStoreKind) -> &'static str {
    match kind {
        BlobStoreKind::Local => "local",
        BlobStoreKind::S3 => "s3",
        BlobStoreKind::Azure => "azure",
        BlobStoreKind::Group => "group",
        _ => "other",
    }
}

fn public_origin(api: &RepoApi, headers: &HeaderMap) -> String {
    if let Some(base) = &api.base_url {
        return base.as_str().trim_end_matches('/').to_owned();
    }
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    format!("{}://{host}", api.fallback_scheme)
}

async fn may_browse(
    api: &RepoApi,
    principal: &Principal,
    repository: &Repository,
) -> Result<bool, ApiError> {
    if admin_of(api, principal).await? {
        return Ok(true);
    }
    allow(api, principal, repository, Action::Browse).await
}

async fn permissions_of(
    api: &RepoApi,
    principal: &Principal,
    repository: &Repository,
    admin: bool,
) -> Result<Permissions, ApiError> {
    if admin {
        return Ok(Permissions {
            browse: true,
            read: true,
            write: true,
            delete: true,
            admin: true,
        });
    }
    Ok(Permissions {
        browse: allow(api, principal, repository, Action::Browse).await?,
        read: allow(api, principal, repository, Action::Read).await?,
        write: allow(api, principal, repository, Action::Write).await?,
        delete: allow(api, principal, repository, Action::Delete).await?,
        admin: false,
    })
}

async fn admin_of(api: &RepoApi, principal: &Principal) -> Result<bool, ApiError> {
    api.rbac.is_admin(principal).await.map_err(ApiError::from)
}

async fn allow(
    api: &RepoApi,
    principal: &Principal,
    repository: &Repository,
    action: Action,
) -> Result<bool, ApiError> {
    let resource = ResourceRef::for_repository(repository, "");
    match api.rbac.check(principal, action, &resource).await {
        Ok(Decision::Allow) => Ok(true),
        Ok(Decision::Deny) => Ok(false),
        Err(error) => Err(ApiError::from(error)),
    }
}

fn invalid(message: &str) -> ApiError {
    ApiError::bad_request("invalid_input", message)
}

fn rejected() -> ApiError {
    ApiError::bad_request("upstream_url_rejected", "The upstream URL is not allowed")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use http::StatusCode;
    use rustryx_api::{AppState, RouterBuilder};
    use rustryx_core::error::AuthError;
    use rustryx_core::identity::{
        AuthContext, AuthMethod, Credentials, Principal, RoleRef, UserSource,
    };
    use rustryx_core::ids::RoleId;
    use rustryx_core::ports::{Authenticator, Decision, RbacChecker};
    use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
    use rustryx_testkit::MemStore;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::RepositoriesModule;

    struct Gate;

    #[async_trait]
    impl RbacChecker for Gate {
        async fn check(
            &self,
            principal: &Principal,
            _: rustryx_core::identity::Action,
            _: &rustryx_core::ports::ResourceRef,
        ) -> Result<Decision, AuthError> {
            Ok(if principal.has_role("nx-admin") {
                Decision::Allow
            } else {
                Decision::Deny
            })
        }

        async fn is_admin(&self, principal: &Principal) -> Result<bool, AuthError> {
            Ok(principal.has_role("nx-admin"))
        }

        async fn can_see_repository(
            &self,
            principal: &Principal,
            _: &str,
            _: rustryx_core::model::RepoFormat,
        ) -> Result<bool, AuthError> {
            Ok(principal.has_role("nx-admin"))
        }
    }

    struct AdminPassword;

    #[async_trait]
    impl Authenticator for AdminPassword {
        async fn authenticate(
            &self,
            credentials: &Credentials,
            _: &AuthContext,
        ) -> Result<Option<Principal>, AuthError> {
            let Credentials::Basic { username, .. } = credentials else {
                return Ok(None);
            };
            if username != "admin" {
                return Err(AuthError::InvalidCredentials);
            }
            let mut principal = Principal::new("admin", UserSource::Local, AuthMethod::BasicLocal);
            principal.roles.push(RoleRef {
                id: RoleId::generate(),
                name: "nx-admin".to_owned(),
            });
            Ok(Some(principal))
        }
    }

    fn router() -> axum::Router {
        let store = MemStore::with_builtin_roles().with_default_blob_store();
        let gate = Arc::new(Gate);
        let module = RepositoriesModule::new(
            Arc::new(store.clone()),
            Arc::new(store),
            gate.clone(),
            Arc::new(rustryx_core::ports::SystemClock),
            super::RepoHttp {
                base_url: None,
                require_https: false,
                anonymous_enabled: false,
                upstream: UpstreamUrlPolicy::any_public(),
            },
        );
        let mut settings = rustryx_api::ApiSettings::default();
        settings.require_https = false;
        let state = AppState::unwired()
            .with_settings(settings)
            .with_authenticator(Arc::new(AdminPassword))
            .with_rbac(gate);
        RouterBuilder::new(state).module(Arc::new(module)).build()
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
        admin: bool,
    ) -> (StatusCode, Value) {
        let mut builder = http::Request::builder().method(method).uri(uri);
        if admin {
            builder = builder.header("authorization", "Basic YWRtaW46eA==");
        }
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder
            .body(Body::from(body.unwrap_or("").to_owned()))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    fn store_id(value: &Value) -> String {
        value["items"][0]["id"]
            .as_str()
            .expect("store id")
            .to_owned()
    }

    #[tokio::test]
    async fn an_administrator_creates_a_hosted_repository_and_lists_it() {
        let app = router();
        let (status, stores) = call(&app, "GET", "/api/v1/blob-stores", None, true).await;
        assert_eq!(status, StatusCode::OK);
        let id = store_id(&stores);
        let body = format!(
            r#"{{"name":"maven-releases","format":"maven2","repo_type":"hosted","blob_store_id":"{id}","format_config":{{"version_policy":"release"}}}}"#
        );
        let (status, created) = call(&app, "POST", "/api/v1/repositories", Some(&body), true).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["name"], "maven-releases");
        assert_eq!(created["write_policy"], "allow_once");
        assert_eq!(created["blob_store"]["name"], "default");
        assert!(
            created["url"]
                .as_str()
                .expect("url")
                .ends_with("/repository/maven-releases/")
        );
        let (status, page) = call(
            &app,
            "GET",
            "/api/v1/repositories?include=stats",
            None,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["items"][0]["name"], "maven-releases");
        assert_eq!(page["items"][0]["stats"]["component_count"], 0);
        assert_eq!(page["items"][0]["health"]["scanned"], false);
        assert_eq!(page["items"][0]["health"]["critical"], 0);
        assert!(page["next"].is_null());
        let (status, hidden) = call(&app, "GET", "/api/v1/repositories", None, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hidden["items"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn a_taken_name_is_already_exists_and_a_loopback_upstream_is_rejected() {
        let app = router();
        let (status, stores) = call(&app, "GET", "/api/v1/blob-stores", None, true).await;
        assert_eq!(status, StatusCode::OK);
        let id = store_id(&stores);
        let body = format!(
            r#"{{"name":"raw-hosted","format":"raw","repo_type":"hosted","blob_store_id":"{id}"}}"#
        );
        assert_eq!(
            call(&app, "POST", "/api/v1/repositories", Some(&body), true)
                .await
                .0,
            StatusCode::CREATED
        );
        let (status, again) = call(&app, "POST", "/api/v1/repositories", Some(&body), true).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(again["error"]["code"], "already_exists");
        let proxy = format!(
            r#"{{"name":"npm-proxy","format":"npm","repo_type":"proxy","blob_store_id":"{id}","proxy":{{"remote_url":"http://127.0.0.1/npm"}}}}"#
        );
        let (status, rejected) =
            call(&app, "POST", "/api/v1/repositories", Some(&proxy), true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(rejected["error"]["code"], "upstream_url_rejected");
    }
}
