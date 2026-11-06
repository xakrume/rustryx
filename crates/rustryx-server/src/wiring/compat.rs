//! Sonatype compat Repository 3 REST API, version 1, under `/service/rest/v1`.
//!
//! The shapes follow the public Sonatype help pages (Repositories API, Components
//! API, Search API): a repository list is a JSON array of name, format, type, url
//! and attributes, and search returns `items` plus `continuationToken`. This is
//! not the native `/api/v1` contract.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::header::HOST;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use http::{HeaderMap, StatusCode};
use rustryx_api::{
    AdminGrant, ApiError, ApiJson, ApiPath, ApiQuery, Authorized, RequestPrincipal, Requires,
    RouteModule, RouteSet,
};
use rustryx_core::error::StoreError;
use rustryx_core::identity::{Action, Principal};
use rustryx_core::ids::RepositoryId;
use rustryx_core::model::{
    GroupConfig, ProxyConfig, RepoFormat, RepoType, Repository, WritePolicy,
};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{
    BlobStoreRepo, Clock, ComponentFilter, ComponentRepo, Decision, PageRequest, RbacChecker,
    RepositoryRepo, ResourceRef,
};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

/// What the compatibility routes need besides the HTTP settings.
pub struct CompatDeps {
    /// Repositories.
    pub repositories: Arc<dyn RepositoryRepo>,
    /// Blob stores, addressed by name in this API.
    pub blob_stores: Arc<dyn BlobStoreRepo>,
    /// Components for search and the components listing.
    pub components: Arc<dyn ComponentRepo>,
    /// Authorization.
    pub rbac: Arc<dyn RbacChecker>,
    /// Clock for new rows.
    pub clock: Arc<dyn Clock>,
    /// `server.base_url`.
    pub base_url: Option<Url>,
    /// Scheme used with the request host when no base URL is set.
    pub require_https: bool,
    /// Same upstream policy as the format engine.
    pub upstream: UpstreamUrlPolicy,
}

/// `/service/rest/v1` compatibility routes.
pub struct CompatModule {
    state: CompatApi,
}

impl CompatModule {
    /// Builds the module.
    #[must_use]
    pub fn new(deps: CompatDeps) -> Self {
        Self {
            state: CompatApi {
                repositories: deps.repositories,
                blob_stores: deps.blob_stores,
                components: deps.components,
                rbac: deps.rbac,
                clock: deps.clock,
                base_url: deps.base_url,
                fallback_scheme: if deps.require_https { "https" } else { "http" },
                upstream: deps.upstream,
            },
        }
    }
}

impl RouteModule for CompatModule {
    fn name(&self) -> &'static str {
        "compat-v1"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let state = self.state.clone();
        routes
            .route(
                "/service/rest/v1/status",
                Requires::Public,
                get(status).with_state(state.clone()),
            )
            .route(
                "/service/rest/v1/repositories",
                Requires::Public,
                get(list_repositories).with_state(state.clone()),
            )
            .route(
                "/service/rest/v1/repositories/{name}",
                Requires::Public,
                get(get_repository).with_state(state.clone()),
            )
            .route(
                "/service/rest/v1/repositories/{name}",
                Requires::Admin,
                delete(delete_repository).with_state(state.clone()),
            )
            .route(
                "/service/rest/v1/repositories/{format}/{kind}",
                Requires::Admin,
                post(create_repository).with_state(state.clone()),
            )
            .route(
                "/service/rest/v1/search",
                Requires::Public,
                get(search).with_state(state.clone()),
            )
            .route(
                "/service/rest/v1/components",
                Requires::Public,
                get(components).with_state(state),
            )
    }
}

#[derive(Clone)]
struct CompatApi {
    repositories: Arc<dyn RepositoryRepo>,
    blob_stores: Arc<dyn BlobStoreRepo>,
    components: Arc<dyn ComponentRepo>,
    rbac: Arc<dyn RbacChecker>,
    clock: Arc<dyn Clock>,
    base_url: Option<Url>,
    fallback_scheme: &'static str,
    upstream: UpstreamUrlPolicy,
}

#[derive(Debug, Deserialize)]
struct NamePath {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RecipePath {
    format: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "q")]
    query: Option<String>,
    #[serde(default, rename = "continuationToken")]
    continuation_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    name: String,
    #[serde(default)]
    online: Option<bool>,
    storage: StorageBody,
    #[serde(default)]
    maven: Option<MavenBody>,
    #[serde(default)]
    proxy: Option<ProxyBody>,
    #[serde(default)]
    group: Option<GroupBody>,
}

#[derive(Debug, Deserialize)]
struct StorageBody {
    #[serde(rename = "blobStoreName")]
    blob_store_name: String,
    #[serde(default, rename = "writePolicy")]
    write_policy: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MavenBody {
    #[serde(default, rename = "versionPolicy")]
    version_policy: Option<String>,
    #[serde(default, rename = "layoutPolicy")]
    layout_policy: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProxyBody {
    #[serde(rename = "remoteUrl")]
    remote_url: String,
    #[serde(default, rename = "contentMaxAge")]
    content_max_age: Option<u64>,
    #[serde(default, rename = "metadataMaxAge")]
    metadata_max_age: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GroupBody {
    #[serde(rename = "memberNames")]
    member_names: Vec<String>,
}

async fn status() -> StatusCode {
    StatusCode::OK
}

async fn list_repositories(
    State(api): State<CompatApi>,
    RequestPrincipal(principal): RequestPrincipal,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, ApiError> {
    let origin = origin_of(&api, &headers);
    let mut items = Vec::new();
    for repository in api.repositories.list().await? {
        if may_browse(&api, &principal, &repository).await? {
            items.push(summary(&repository, &origin));
        }
    }
    items.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(Json(items))
}

async fn get_repository(
    State(api): State<CompatApi>,
    RequestPrincipal(principal): RequestPrincipal,
    ApiPath(path): ApiPath<NamePath>,
    headers: HeaderMap,
) -> Result<Json<Value>, CompatFail> {
    let repository = visible(&api, &principal, &path.name).await?;
    Ok(Json(summary(&repository, &origin_of(&api, &headers))))
}

async fn delete_repository(
    State(api): State<CompatApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<NamePath>,
) -> Result<StatusCode, CompatFail> {
    let name = parse_name(&path.name)?;
    let Some(repository) = api.repositories.get_by_name(&name).await.map_err(store)? else {
        return Err(missing());
    };
    api.repositories
        .delete(repository.id)
        .await
        .map_err(store)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_repository(
    State(api): State<CompatApi>,
    _proof: Authorized<AdminGrant>,
    ApiPath(path): ApiPath<RecipePath>,
    ApiJson(body): ApiJson<CreateBody>,
) -> Result<StatusCode, CompatFail> {
    let repository = build(&api, &path, body).await?;
    match api.repositories.create(&repository).await {
        Ok(()) => Ok(StatusCode::CREATED),
        Err(StoreError::Conflict(_)) => {
            Err(fail(StatusCode::BAD_REQUEST, "Repository already exists"))
        }
        Err(error) => Err(store(error)),
    }
}

async fn search(
    State(api): State<CompatApi>,
    RequestPrincipal(principal): RequestPrincipal,
    ApiQuery(query): ApiQuery<SearchQuery>,
) -> Result<Json<Value>, CompatFail> {
    let page = find_components(&api, &principal, &query, false).await?;
    Ok(Json(page))
}

async fn components(
    State(api): State<CompatApi>,
    RequestPrincipal(principal): RequestPrincipal,
    ApiQuery(query): ApiQuery<SearchQuery>,
) -> Result<Json<Value>, CompatFail> {
    if query.repository.is_none() {
        return Err(fail(StatusCode::BAD_REQUEST, "repository is required"));
    }
    Ok(Json(find_components(&api, &principal, &query, true).await?))
}

async fn find_components(
    api: &CompatApi,
    principal: &Principal,
    query: &SearchQuery,
    require_visible: bool,
) -> Result<Value, CompatFail> {
    let repository_id = match &query.repository {
        Some(name) => Some(visible(api, principal, name).await?.id),
        None => None,
    };
    if require_visible && repository_id.is_none() {
        return Err(fail(StatusCode::BAD_REQUEST, "repository is required"));
    }
    let filter = ComponentFilter {
        repository_id,
        group_id: query.group.clone(),
        name: query.name.clone(),
        version: query.version.clone(),
        query: query.query.clone(),
    };
    let page = api
        .components
        .list(
            &filter,
            &PageRequest {
                limit: 100,
                after: query.continuation_token.clone(),
            },
        )
        .await
        .map_err(store)?;
    let repos = api.repositories.list().await.map_err(store)?;
    let items = page
        .items
        .iter()
        .filter_map(|component| {
            let repository = repos
                .iter()
                .find(|repository| repository.id == component.repository_id)?;
            Some(json!({
                "id": component.id.to_string(),
                "repository": repository.name.as_str(),
                "format": component.format.as_str(),
                "group": component.group_id,
                "name": component.name,
                "version": component.version,
                "assets": [],
            }))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "items": items,
        "continuationToken": page.next,
    }))
}

async fn build(
    api: &CompatApi,
    path: &RecipePath,
    body: CreateBody,
) -> Result<Repository, CompatFail> {
    let format = recipe(&path.format)?;
    let kind = recipe_kind(&path.kind)?;
    let name = parse_name(&body.name)?;
    let store = api
        .blob_stores
        .get_by_name(&body.storage.blob_store_name)
        .await
        .map_err(store)?
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "Unknown blob store"))?;
    let mut repository = Repository::new(
        RepositoryId::generate(),
        name,
        format,
        kind,
        store.id,
        api.clock.now(),
    );
    repository.online = body.online.unwrap_or(true);
    if let Some(maven) = &body.maven {
        repository.format_config = maven_config(maven);
    }
    match kind {
        RepoType::Hosted => {
            if body.proxy.is_some() || body.group.is_some() {
                return Err(fail(
                    StatusCode::BAD_REQUEST,
                    "A hosted repository has no proxy or group settings",
                ));
            }
            repository.write_policy = write_policy(body.storage.write_policy.as_deref())?;
        }
        RepoType::Proxy => proxy_into(api, &mut repository, body.proxy)?,
        RepoType::Group => group_into(api, &mut repository, body.group).await?,
    }
    Ok(repository)
}

fn proxy_into(
    api: &CompatApi,
    repository: &mut Repository,
    proxy: Option<ProxyBody>,
) -> Result<(), CompatFail> {
    let Some(proxy) = proxy else {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "A proxy repository needs remoteUrl",
        ));
    };
    let url = Url::parse(&proxy.remote_url)
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "The upstream URL is not allowed"))?;
    if api.upstream.check_url(&url).is_err() {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "The upstream URL is not allowed",
        ));
    }
    let mut config = ProxyConfig::new(proxy.remote_url);
    if let Some(minutes) = proxy.content_max_age {
        config.content_max_age_secs = minutes.saturating_mul(60);
    }
    if let Some(minutes) = proxy.metadata_max_age {
        config.metadata_max_age_secs = minutes.saturating_mul(60);
    }
    repository.proxy = Some(config);
    repository.write_policy = WritePolicy::Deny;
    Ok(())
}

async fn group_into(
    api: &CompatApi,
    repository: &mut Repository,
    group: Option<GroupBody>,
) -> Result<(), CompatFail> {
    let Some(group) = group.filter(|group| !group.member_names.is_empty()) else {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "A group repository needs memberNames",
        ));
    };
    let mut members = Vec::new();
    for raw in group.member_names {
        let name = parse_name(&raw)?;
        if name == repository.name {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "A group cannot contain itself",
            ));
        }
        let Some(existing) = api.repositories.get_by_name(&name).await.map_err(store)? else {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "A group member does not exist",
            ));
        };
        if existing.format != repository.format {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "A group member must use the same format",
            ));
        }
        members.push(name);
    }
    repository.group = Some(GroupConfig { members });
    repository.write_policy = WritePolicy::Deny;
    Ok(())
}

fn summary(repository: &Repository, origin: &str) -> Value {
    let mut attributes = serde_json::Map::new();
    if let Some(proxy) = &repository.proxy {
        attributes.insert("proxy".to_owned(), json!({ "remoteUrl": proxy.remote_url }));
    }
    if let Some(group) = &repository.group {
        let names: Vec<&str> = group.members.iter().map(RepoName::as_str).collect();
        attributes.insert("group".to_owned(), json!({ "memberNames": names }));
    }
    json!({
        "name": repository.name.as_str(),
        "format": repository.format.as_str(),
        "type": repository.repo_type.as_str(),
        "url": format!("{origin}/repository/{}", repository.name),
        "size": 0,
        "attributes": attributes,
    })
}

fn maven_config(maven: &MavenBody) -> Value {
    let mut config = serde_json::Map::new();
    if let Some(policy) = &maven.version_policy {
        config.insert(
            "version_policy".to_owned(),
            Value::String(policy.to_ascii_lowercase()),
        );
    }
    if let Some(policy) = &maven.layout_policy {
        config.insert(
            "layout_policy".to_owned(),
            Value::String(policy.to_ascii_lowercase()),
        );
    }
    Value::Object(config)
}

fn recipe(format: &str) -> Result<RepoFormat, CompatFail> {
    match format {
        "maven" | "maven2" => Ok(RepoFormat::Maven2),
        "npm" => Ok(RepoFormat::Npm),
        "pypi" => Ok(RepoFormat::Pypi),
        "docker" => Ok(RepoFormat::Docker),
        "oci" => Ok(RepoFormat::Oci),
        "helm" => Ok(RepoFormat::Helm),
        "raw" => Ok(RepoFormat::Raw),
        "pub" => Ok(RepoFormat::Pub),
        "rpm" | "yum" | "dnf" => Ok(RepoFormat::Rpm),
        "apt" | "deb" => Ok(RepoFormat::Apt),
        "apk" | "alpine" => Ok(RepoFormat::Apk),
        _ => Err(fail(StatusCode::BAD_REQUEST, "Unknown repository format")),
    }
}

fn recipe_kind(kind: &str) -> Result<RepoType, CompatFail> {
    match kind {
        "hosted" => Ok(RepoType::Hosted),
        "proxy" => Ok(RepoType::Proxy),
        "group" => Ok(RepoType::Group),
        _ => Err(fail(StatusCode::BAD_REQUEST, "Unknown repository type")),
    }
}

fn write_policy(raw: Option<&str>) -> Result<WritePolicy, CompatFail> {
    match raw.unwrap_or("allow_once").to_ascii_lowercase().as_str() {
        "allow" => Ok(WritePolicy::Allow),
        "allow_once" => Ok(WritePolicy::AllowOnce),
        "deny" => Ok(WritePolicy::Deny),
        _ => Err(fail(StatusCode::BAD_REQUEST, "Unknown writePolicy")),
    }
}

fn parse_name(raw: &str) -> Result<RepoName, CompatFail> {
    RepoName::parse(raw).map_err(|_| fail(StatusCode::BAD_REQUEST, "Invalid repository name"))
}

async fn visible(
    api: &CompatApi,
    principal: &Principal,
    raw: &str,
) -> Result<Repository, CompatFail> {
    let name = parse_name(raw)?;
    let Some(repository) = api.repositories.get_by_name(&name).await.map_err(store)? else {
        return Err(missing());
    };
    if may_browse(api, principal, &repository)
        .await
        .map_err(|error| CompatFail::Stored(Box::new(error)))?
    {
        Ok(repository)
    } else {
        Err(missing())
    }
}

async fn may_browse(
    api: &CompatApi,
    principal: &Principal,
    repository: &Repository,
) -> Result<bool, ApiError> {
    if api.rbac.is_admin(principal).await.map_err(ApiError::from)? {
        return Ok(true);
    }
    let resource = ResourceRef::for_repository(repository, "");
    match api.rbac.check(principal, Action::Browse, &resource).await {
        Ok(Decision::Allow) => Ok(true),
        Ok(Decision::Deny) => Ok(false),
        Err(error) => Err(ApiError::from(error)),
    }
}

fn origin_of(api: &CompatApi, headers: &HeaderMap) -> String {
    if let Some(base) = &api.base_url {
        return base.as_str().trim_end_matches('/').to_owned();
    }
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    format!("{}://{host}", api.fallback_scheme)
}

enum CompatFail {
    Message(StatusCode, &'static str),
    Stored(Box<ApiError>),
}

impl IntoResponse for CompatFail {
    fn into_response(self) -> Response {
        match self {
            Self::Message(status, message) => {
                (status, Json(json!({ "message": message }))).into_response()
            }
            Self::Stored(error) => error.into_response(),
        }
    }
}

fn fail(status: StatusCode, message: &'static str) -> CompatFail {
    CompatFail::Message(status, message)
}

fn missing() -> CompatFail {
    fail(StatusCode::NOT_FOUND, "Repository not found")
}

fn store(error: StoreError) -> CompatFail {
    CompatFail::Stored(Box::new(ApiError::from(error)))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use http::StatusCode;
    use rustryx_api::{ApiSettings, AppState, RouterBuilder};
    use rustryx_core::error::AuthError;
    use rustryx_core::identity::{
        AuthContext, AuthMethod, Credentials, Principal, RoleRef, UserSource,
    };
    use rustryx_core::ids::RoleId;
    use rustryx_core::ports::{Authenticator, Decision, RbacChecker, SystemClock};
    use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
    use rustryx_testkit::MemStore;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::{CompatDeps, CompatModule};

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
        let module = CompatModule::new(CompatDeps {
            repositories: Arc::new(store.clone()),
            blob_stores: Arc::new(store.clone()),
            components: Arc::new(store),
            rbac: gate.clone(),
            clock: Arc::new(SystemClock),
            base_url: None,
            require_https: false,
            upstream: UpstreamUrlPolicy::any_public(),
        });
        let mut settings = ApiSettings::default();
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
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    #[tokio::test]
    async fn maven_hosted_is_created_and_listed_in_the_compat_shape() {
        let app = router();
        let body = r#"{"name":"maven-releases","online":true,"storage":{"blobStoreName":"default","writePolicy":"ALLOW_ONCE"},"maven":{"versionPolicy":"RELEASE","layoutPolicy":"STRICT"}}"#;
        let (status, _) = call(
            &app,
            "POST",
            "/service/rest/v1/repositories/maven/hosted",
            Some(body),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, list) = call(&app, "GET", "/service/rest/v1/repositories", None, true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list[0]["name"], "maven-releases");
        assert_eq!(list[0]["format"], "maven2");
        assert_eq!(list[0]["type"], "hosted");
        assert!(
            list[0]["url"]
                .as_str()
                .expect("url")
                .ends_with("/repository/maven-releases")
        );
        let (status, hidden) =
            call(&app, "GET", "/service/rest/v1/repositories", None, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hidden.as_array().map(Vec::len), Some(0));
        let (status, page) = call(&app, "GET", "/service/rest/v1/search", None, true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["items"].as_array().map(Vec::len), Some(0));
        assert!(page["continuationToken"].is_null());
    }

    #[tokio::test]
    async fn must_not_accept_a_loopback_proxy_or_a_duplicate_name() {
        let app = router();
        let proxy = r#"{"name":"maven-central","storage":{"blobStoreName":"default"},"proxy":{"remoteUrl":"http://127.0.0.1/maven2"}}"#;
        let (status, rejected) = call(
            &app,
            "POST",
            "/service/rest/v1/repositories/maven/proxy",
            Some(proxy),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(rejected["message"], "The upstream URL is not allowed");
        let hosted = r#"{"name":"raw-hosted","storage":{"blobStoreName":"default"}}"#;
        assert_eq!(
            call(
                &app,
                "POST",
                "/service/rest/v1/repositories/raw/hosted",
                Some(hosted),
                true
            )
            .await
            .0,
            StatusCode::CREATED
        );
        let (status, again) = call(
            &app,
            "POST",
            "/service/rest/v1/repositories/raw/hosted",
            Some(hosted),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(again["message"], "Repository already exists");
        let (status, _) = call(&app, "GET", "/service/rest/v1/status", None, false).await;
        assert_eq!(status, StatusCode::OK);
    }
}
