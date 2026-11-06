//! `GET /api/v1/repositories/{name}/tree` and `GET /api/v1/components`.
//!
//! The tree is one folder level. A proxy shows cached assets only, so a proxy that
//! has not been read yet is an empty folder, not a 404. Docker and OCI have no
//! path tree: `409 unsupported_for_format`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::routing::{delete, get};
use http::{HeaderMap, StatusCode};
use rustryx_api::{ApiError, ApiPath, ApiQuery, RequestPrincipal, Requires, RouteModule, RouteSet};
use rustryx_core::error::StoreError;
use rustryx_core::identity::{Action, Principal};
use rustryx_core::ids::ComponentId;
use rustryx_core::model::{Asset, Component, RepoFormat, RepoType, Repository};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{
    AssetRepo, ComponentFilter, ComponentRepo, Decision, PageRequest, RbacChecker, RepositoryRepo,
    ResourceRef,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const ASSET_PAGE: u32 = 500;
const ASSET_CAP: usize = 2_000;

/// Browse routes over the metadata store.
pub struct BrowseModule {
    state: BrowseApi,
}

impl BrowseModule {
    /// `repositories` decides visibility. `assets` and `components` are what a folder lists.
    #[must_use]
    pub fn new(
        repositories: Arc<dyn RepositoryRepo>,
        assets: Arc<dyn AssetRepo>,
        components: Arc<dyn ComponentRepo>,
        rbac: Arc<dyn RbacChecker>,
        http: crate::wiring::client_setup::SetupHttp,
    ) -> Self {
        Self {
            state: BrowseApi {
                repositories,
                assets,
                components,
                rbac,
                http,
            },
        }
    }
}

impl RouteModule for BrowseModule {
    fn name(&self) -> &'static str {
        "browse"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let state = self.state.clone();
        routes
            .route(
                "/api/v1/repositories/{name}/tree",
                Requires::Public,
                get(tree).with_state(state.clone()),
            )
            .route(
                "/api/v1/components",
                Requires::Public,
                get(components).with_state(state.clone()),
            )
            .route(
                "/api/v1/components/{component_id}",
                Requires::Admin,
                delete(delete_component).with_state(state.clone()),
            )
            .route(
                "/api/v1/client-setup",
                Requires::Public,
                get(client_setup).with_state(state),
            )
    }
}

#[derive(Clone)]
struct BrowseApi {
    repositories: Arc<dyn RepositoryRepo>,
    assets: Arc<dyn AssetRepo>,
    components: Arc<dyn ComponentRepo>,
    rbac: Arc<dyn RbacChecker>,
    http: crate::wiring::client_setup::SetupHttp,
}

#[derive(Debug, Deserialize)]
struct NamePath {
    name: RepoName,
}

#[derive(Debug, Deserialize)]
struct TreeQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    after: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ComponentQuery {
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Serialize)]
struct TreePage {
    path: String,
    items: Vec<TreeNode>,
    next: Option<String>,
}

#[derive(Serialize)]
struct TreeNode {
    name: String,
    path: String,
    kind: &'static str,
    asset_id: Option<rustryx_core::ids::AssetId>,
    component_id: Option<rustryx_core::ids::ComponentId>,
    size_bytes: Option<u64>,
    content_type: Option<String>,
    #[serde(with = "rustryx_core::rfc3339::option")]
    updated_at: Option<OffsetDateTime>,
    child_count: Option<u64>,
    source_repository: Option<String>,
}

#[derive(Serialize)]
struct ComponentPage {
    items: Vec<ComponentView>,
    next: Option<String>,
}

#[derive(Serialize)]
struct ComponentView {
    id: rustryx_core::ids::ComponentId,
    repository_id: rustryx_core::ids::RepositoryId,
    format: RepoFormat,
    group_id: Option<String>,
    name: String,
    version: Option<String>,
    #[serde(with = "rustryx_core::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "rustryx_core::rfc3339")]
    updated_at: OffsetDateTime,
    repository: String,
    asset_count: u64,
    size_bytes: u64,
    attributes_summary: serde_json::Value,
}

async fn tree(
    State(api): State<BrowseApi>,
    RequestPrincipal(principal): RequestPrincipal,
    ApiPath(path): ApiPath<NamePath>,
    ApiQuery(query): ApiQuery<TreeQuery>,
) -> Result<Json<TreePage>, ApiError> {
    let repository = visible(&api, &principal, &path.name).await?;
    if matches!(repository.format, RepoFormat::Docker | RepoFormat::Oci) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "unsupported_for_format",
            "This format has no file tree",
        ));
    }
    let folder = folder_of(query.path.as_deref())?;
    let assets = assets_under(&api, &repository, &folder).await?;
    let mut nodes = children(&folder, &assets, query.q.as_deref());
    let limit = usize::try_from(query.limit.unwrap_or(200).clamp(1, 500)).unwrap_or(200);
    if let Some(after) = query.after.as_deref() {
        let Some(position) = nodes.iter().position(|node| node.path == after) else {
            return Err(ApiError::bad_request(
                "invalid_page_token",
                "The page token is malformed or was issued for another request",
            ));
        };
        nodes = nodes.split_off(position.saturating_add(1));
    }
    let more = nodes.len() > limit;
    nodes.truncate(limit);
    let next = more
        .then(|| nodes.last().map(|node| node.path.clone()))
        .flatten();
    Ok(Json(TreePage {
        path: folder.display,
        items: nodes,
        next,
    }))
}

#[derive(Debug, Deserialize)]
struct SetupQuery {
    repository: String,
}

async fn client_setup(
    State(api): State<BrowseApi>,
    RequestPrincipal(principal): RequestPrincipal,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<SetupQuery>,
) -> Result<Json<crate::wiring::client_setup::Setup>, ApiError> {
    let name = RepoName::parse(&query.repository)
        .map_err(|_| ApiError::bad_request("invalid_query", "Invalid repository name"))?;
    let repository = visible(&api, &principal, &name).await?;
    let body = crate::wiring::client_setup::build(
        &api.repositories,
        &api.rbac,
        &api.http,
        &headers,
        &principal,
        &repository,
    )
    .await?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
struct ComponentPath {
    component_id: ComponentId,
}

async fn delete_component(
    State(api): State<BrowseApi>,
    ApiPath(path): ApiPath<ComponentPath>,
) -> Result<StatusCode, ApiError> {
    let Some(component) = api.components.get(path.component_id).await? else {
        return Err(ApiError::route_not_found());
    };
    let Some(repository) = api.repositories.get(component.repository_id).await? else {
        return Err(ApiError::route_not_found());
    };
    if repository.repo_type == RepoType::Group {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "repository_not_hosted",
            "A group repository has no files of its own",
        ));
    }
    let assets = api.assets.list_by_component(component.id).await?;
    for asset in &assets {
        if let Err(error) = super::rpm_sync::remember_sync_exclusion(
            api.repositories.as_ref(),
            asset.repository_id,
            &asset.path,
        )
        .await
        {
            tracing::error!(%error, "sync exclusion was not stored");
        }
    }
    match api.components.delete(component.id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(StoreError::NotFound(_)) => Err(ApiError::route_not_found()),
        Err(StoreError::Conflict(_)) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "immutable_tag",
            "The component is protected",
        )),
        Err(error) => Err(error.into()),
    }
}

async fn components(
    State(api): State<BrowseApi>,
    RequestPrincipal(principal): RequestPrincipal,
    ApiQuery(query): ApiQuery<ComponentQuery>,
) -> Result<Json<ComponentPage>, ApiError> {
    let Some(name) = query.repository.as_deref() else {
        return Err(ApiError::bad_request(
            "invalid_query",
            "repository is required",
        ));
    };
    let name = RepoName::parse(name)
        .map_err(|_| ApiError::bad_request("invalid_query", "Invalid repository name"))?;
    let repository = visible(&api, &principal, &name).await?;
    let page = api
        .components
        .list(
            &ComponentFilter {
                repository_id: Some(repository.id),
                group_id: query.group,
                name: query.name,
                version: query.version,
                query: query.q,
            },
            &PageRequest {
                limit: query.limit.unwrap_or(50).clamp(1, 200),
                after: query.after,
            },
        )
        .await?;
    let items = page
        .items
        .into_iter()
        .map(|component| component_view(&repository, component))
        .collect();
    Ok(Json(ComponentPage {
        items,
        next: page.next,
    }))
}

struct Folder {
    /// Asset prefix, without a leading slash. The root is empty. A nested folder ends with `/`.
    prefix: String,
    /// The path the client sent, starting and ending with `/`.
    display: String,
}

fn folder_of(raw: Option<&str>) -> Result<Folder, ApiError> {
    let raw = raw.unwrap_or("/");
    if !raw.starts_with('/') || raw.contains('\\') {
        return Err(ApiError::bad_request(
            "invalid_query",
            "path must start with /",
        ));
    }
    let body = raw.trim_matches('/');
    let unsafe_segment = !body.is_empty()
        && body
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if unsafe_segment {
        return Err(ApiError::bad_request(
            "invalid_query",
            "path is not a folder",
        ));
    }
    if body.is_empty() {
        return Ok(Folder {
            prefix: String::new(),
            display: "/".to_owned(),
        });
    }
    Ok(Folder {
        prefix: format!("{body}/"),
        display: format!("/{body}/"),
    })
}

async fn assets_under(
    api: &BrowseApi,
    repository: &Repository,
    folder: &Folder,
) -> Result<Vec<(Asset, Option<String>)>, ApiError> {
    let grouped = repository.repo_type == RepoType::Group;
    let mut sources = Vec::new();
    if grouped {
        let members = repository
            .group
            .as_ref()
            .map(|group| group.members.clone())
            .unwrap_or_default();
        for member in members {
            if let Some(existing) = api.repositories.get_by_name(&member).await? {
                sources.push(existing);
            }
        }
    } else {
        sources.push(repository.clone());
    }
    let mut seen = BTreeMap::new();
    for source in sources {
        let label = grouped.then(|| source.name.to_string());
        for asset in collect(&api.assets, source.id, &folder.prefix).await? {
            seen.entry(asset.path.clone())
                .or_insert((asset, label.clone()));
        }
    }
    Ok(seen.into_values().collect())
}

async fn collect(
    assets: &Arc<dyn AssetRepo>,
    repository: rustryx_core::ids::RepositoryId,
    prefix: &str,
) -> Result<Vec<Asset>, ApiError> {
    let mut after = None;
    let mut collected = Vec::new();
    loop {
        let page = assets
            .list(
                repository,
                prefix,
                &PageRequest {
                    limit: ASSET_PAGE,
                    after: after.clone(),
                },
            )
            .await?;
        let finished = page.next.is_none() || page.items.len() < ASSET_PAGE as usize;
        collected.extend(page.items);
        if finished || collected.len() >= ASSET_CAP {
            collected.truncate(ASSET_CAP);
            return Ok(collected);
        }
        after = page.next;
    }
}

fn children(
    folder: &Folder,
    assets: &[(Asset, Option<String>)],
    query: Option<&str>,
) -> Vec<TreeNode> {
    let mut folders = BTreeSet::new();
    let mut files: BTreeMap<String, usize> = BTreeMap::new();
    for (index, (asset, _)) in assets.iter().enumerate() {
        let Some(rest) = asset.path.strip_prefix(&folder.prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let (name, nested) = rest
            .split_once('/')
            .map_or((rest, false), |(name, _)| (name, true));
        if name.is_empty() {
            continue;
        }
        if nested {
            folders.insert(name.to_owned());
            files.remove(name);
        } else if !folders.contains(name) {
            files.entry(name.to_owned()).or_insert(index);
        }
    }
    let needle = query.map(str::to_ascii_lowercase);
    let mut nodes = Vec::new();
    for name in folders {
        if prefix_match(&name, needle.as_deref()) {
            nodes.push(folder_node(folder, &name));
        }
    }
    for (name, index) in files {
        if prefix_match(&name, needle.as_deref()) {
            let (asset, source) = &assets[index];
            nodes.push(file_node(folder, &name, asset, source.as_ref()));
        }
    }
    nodes
}

fn node_path(folder: &Folder, name: &str, directory: bool) -> String {
    let base = folder.display.trim_end_matches('/');
    if directory {
        format!("{base}/{name}/")
    } else {
        format!("{base}/{name}")
    }
}

fn folder_node(folder: &Folder, name: &str) -> TreeNode {
    TreeNode {
        path: node_path(folder, name, true),
        name: name.to_owned(),
        kind: "folder",
        asset_id: None,
        component_id: None,
        size_bytes: None,
        content_type: None,
        updated_at: None,
        child_count: None,
        source_repository: None,
    }
}

fn file_node(folder: &Folder, name: &str, asset: &Asset, source: Option<&String>) -> TreeNode {
    TreeNode {
        path: node_path(folder, name, false),
        name: name.to_owned(),
        kind: "asset",
        asset_id: Some(asset.id),
        component_id: asset.component_id,
        size_bytes: Some(asset.size_bytes),
        content_type: Some(asset.content_type.clone()),
        updated_at: Some(asset.updated_at),
        child_count: None,
        source_repository: source.cloned(),
    }
}

fn prefix_match(name: &str, needle: Option<&str>) -> bool {
    needle.is_none_or(|needle| name.to_ascii_lowercase().starts_with(needle))
}

fn component_view(repository: &Repository, component: Component) -> ComponentView {
    ComponentView {
        id: component.id,
        repository_id: component.repository_id,
        format: component.format,
        group_id: component.group_id,
        name: component.name,
        version: component.version,
        created_at: component.created_at,
        updated_at: component.updated_at,
        repository: repository.name.to_string(),
        asset_count: 0,
        size_bytes: 0,
        attributes_summary: serde_json::json!({}),
    }
}

async fn visible(
    api: &BrowseApi,
    principal: &Principal,
    name: &RepoName,
) -> Result<Repository, ApiError> {
    let Some(repository) = api.repositories.get_by_name(name).await? else {
        return Err(ApiError::route_not_found());
    };
    let allowed = if api.rbac.is_admin(principal).await.map_err(ApiError::from)? {
        true
    } else {
        let resource = ResourceRef::for_repository(&repository, "");
        match api.rbac.check(principal, Action::Browse, &resource).await {
            Ok(Decision::Allow) => true,
            Ok(Decision::Deny) => false,
            Err(error) => return Err(ApiError::from(error)),
        }
    };
    if allowed {
        Ok(repository)
    } else {
        Err(ApiError::route_not_found())
    }
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
    use rustryx_core::model::{RepoFormat, RepoType};
    use rustryx_core::ports::{Authenticator, Decision, RbacChecker, RepositoryRepo};
    use rustryx_testkit::MemStore;
    use rustryx_testkit::fixtures::repository;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::BrowseModule;

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
            _: RepoFormat,
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

    async fn app() -> axum::Router {
        let store = MemStore::with_builtin_roles().with_default_blob_store();
        RepositoryRepo::create(
            &store,
            &repository("pub.dev", RepoFormat::Pub, RepoType::Proxy),
        )
        .await
        .expect("proxy");
        RepositoryRepo::create(
            &store,
            &repository("docker-hosted", RepoFormat::Docker, RepoType::Hosted),
        )
        .await
        .expect("docker");
        let gate = Arc::new(Gate);
        let module = BrowseModule::new(
            Arc::new(store.clone()),
            Arc::new(store.clone()),
            Arc::new(store),
            gate.clone(),
            crate::wiring::client_setup::SetupHttp {
                base_url: None,
                fallback_scheme: "http",
                anonymous_enabled: false,
            },
        );
        let mut settings = ApiSettings::default();
        settings.require_https = false;
        let state = AppState::unwired()
            .with_settings(settings)
            .with_authenticator(Arc::new(AdminPassword))
            .with_rbac(gate);
        RouterBuilder::new(state).module(Arc::new(module)).build()
    }

    async fn get(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
        let request = http::Request::builder()
            .uri(uri)
            .header("authorization", "Basic YWRtaW46eA==")
            .body(Body::empty())
            .expect("request");
        let response = router.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn an_empty_proxy_folder_is_an_empty_page() {
        let router = app().await;
        let (status, body) = get(&router, "/api/v1/repositories/pub.dev/tree?path=/").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["path"], "/");
        assert_eq!(body["items"].as_array().map(Vec::len), Some(0));
        assert!(body["next"].is_null());
        let (status, page) = get(&router, "/api/v1/components?repository=pub.dev").await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["items"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn must_not_offer_a_file_tree_for_docker() {
        let router = app().await;
        let (status, body) = get(&router, "/api/v1/repositories/docker-hosted/tree?path=/").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "unsupported_for_format");
    }

    #[tokio::test]
    async fn pub_proxy_setup_explains_dart_and_does_not_offer_publish() {
        let router = app().await;
        let (status, body) = get(&router, "/api/v1/client-setup?repository=pub.dev").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["variant"], "authenticated");
        assert!(body["publish_target"].is_null());
        let snippets = body["snippets"].as_array().expect("snippets");
        assert!(
            snippets
                .iter()
                .any(|snippet| snippet["id"] == "dart-hosted")
        );
        assert!(
            snippets
                .iter()
                .all(|snippet| snippet["purpose"] != "publish")
        );
        assert!(
            snippets
                .iter()
                .all(|snippet| !snippet["content"].as_str().unwrap_or("").contains("ryx_"))
        );
    }
}
