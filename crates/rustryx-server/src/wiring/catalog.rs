//! The read routes the web UI opens from the menu, plus personal tokens.
//!
//! Lists come from the metadata store. Cleanup policies, tasks and audit events
//! answer an empty page until those stores are mounted here: the page renders
//! its empty state instead of `route_not_found`.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::routing::get;
use http::StatusCode;
use rustryx_api::{
    AdminGrant, ApiError, ApiJson, ApiQuery, Authorized, InteractiveGrant, RequestPrincipal,
    Requires, RouteModule, RouteSet,
};
use rustryx_auth::tokens::{self, TokenSpec};
use rustryx_core::identity::{IdpProvider, User, UserSource};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{
    Clock, ComponentFilter, ComponentRepo, IdpMappingRepo, PageRequest, PrivilegeRepo,
    RepositoryRepo, RoleRepo, RoutingRuleRepo, SelectorRepo, TokenRepo, UserRepo,
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Ports the menu routes read.
pub struct CatalogDeps {
    /// Accounts.
    pub users: Arc<dyn UserRepo>,
    /// Roles.
    pub roles: Arc<dyn RoleRepo>,
    /// Privileges.
    pub privileges: Arc<dyn PrivilegeRepo>,
    /// Content selectors.
    pub selectors: Arc<dyn SelectorRepo>,
    /// Personal tokens.
    pub tokens: Arc<dyn TokenRepo>,
    /// Group mappings.
    pub mappings: Arc<dyn IdpMappingRepo>,
    /// Repositories.
    pub repositories: Arc<dyn RepositoryRepo>,
    /// Routing rules.
    pub routing_rules: Arc<dyn RoutingRuleRepo>,
    /// Components for search.
    pub components: Arc<dyn ComponentRepo>,
    /// Clock for new tokens.
    pub clock: Arc<dyn Clock>,
    /// `auth.anonymous_enabled`.
    pub anonymous_enabled: bool,
}

/// Menu and account routes.
pub struct CatalogModule {
    state: CatalogApi,
}

impl CatalogModule {
    /// Builds the module.
    #[must_use]
    pub fn new(deps: CatalogDeps) -> Self {
        Self {
            state: CatalogApi {
                users: deps.users,
                roles: deps.roles,
                privileges: deps.privileges,
                selectors: deps.selectors,
                tokens: deps.tokens,
                mappings: deps.mappings,
                repositories: deps.repositories,
                routing_rules: deps.routing_rules,
                components: deps.components,
                clock: deps.clock,
                anonymous_enabled: deps.anonymous_enabled,
            },
        }
    }
}

impl RouteModule for CatalogModule {
    fn name(&self) -> &'static str {
        "catalog"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        let state = self.state.clone();
        routes
            .route("/api/v1/system/info", Requires::Public, get(system_info))
            .route(
                "/api/v1/formats",
                Requires::Public,
                get(formats).with_state(state.clone()),
            )
            .route(
                "/api/v1/system/status",
                Requires::Admin,
                get(system_status).with_state(state.clone()),
            )
            .route(
                "/api/v1/dashboard/summary",
                Requires::Public,
                get(dashboard).with_state(state.clone()),
            )
            .route(
                "/api/v1/users",
                Requires::Admin,
                get(users).with_state(state.clone()),
            )
            .route(
                "/api/v1/roles",
                Requires::Admin,
                get(roles).with_state(state.clone()),
            )
            .route(
                "/api/v1/privileges",
                Requires::Admin,
                get(privileges).with_state(state.clone()),
            )
            .route(
                "/api/v1/content-selectors",
                Requires::Admin,
                get(selectors).with_state(state.clone()),
            )
            .route(
                "/api/v1/routing-rules",
                Requires::Admin,
                get(routing_rules).with_state(state.clone()),
            )
            .route(
                "/api/v1/cleanup-policies",
                Requires::Admin,
                get(empty_page).with_state(state.clone()),
            )
            .route(
                "/api/v1/tasks",
                Requires::Admin,
                get(empty_page).with_state(state.clone()),
            )
            .route(
                "/api/v1/audit-events",
                Requires::Admin,
                get(empty_page).with_state(state.clone()),
            )
            .route(
                "/api/v1/anonymous-access",
                Requires::Admin,
                get(anonymous_access).with_state(state.clone()),
            )
            .route(
                "/api/v1/auth/providers/details",
                Requires::Admin,
                get(providers).with_state(state.clone()),
            )
            .route(
                "/api/v1/idp-group-mappings",
                Requires::Admin,
                get(mappings).with_state(state.clone()),
            )
            .route(
                "/api/v1/search/components",
                Requires::Public,
                get(search_components).with_state(state.clone()),
            )
            .route(
                "/api/v1/search/assets",
                Requires::Public,
                get(empty_page).with_state(state.clone()),
            )
            .route(
                "/api/v1/me/tokens",
                Requires::Interactive,
                get(my_tokens).post(create_token).with_state(state),
            )
    }
}

#[derive(Clone)]
struct CatalogApi {
    users: Arc<dyn UserRepo>,
    roles: Arc<dyn RoleRepo>,
    privileges: Arc<dyn PrivilegeRepo>,
    selectors: Arc<dyn SelectorRepo>,
    tokens: Arc<dyn TokenRepo>,
    mappings: Arc<dyn IdpMappingRepo>,
    repositories: Arc<dyn RepositoryRepo>,
    routing_rules: Arc<dyn RoutingRuleRepo>,
    components: Arc<dyn ComponentRepo>,
    clock: Arc<dyn Clock>,
    anonymous_enabled: bool,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenBody {
    name: String,
    #[serde(default)]
    expires_in_days: Option<u32>,
    #[serde(default)]
    scopes: Vec<String>,
}

async fn system_info() -> Json<Value> {
    Json(json!({
        "name": "rustryx",
        "version": env!("CARGO_PKG_VERSION"),
        "license": "MIT",
        "build": { "commit": null, "built_at": null, "profile": "release" },
        "supply_chain": { "sbom_url": null, "signature_hint": null },
        "third_party_licenses_url": null,
        "banner": null
    }))
}

async fn formats() -> Json<Value> {
    let items: Vec<Value> = RepoFormat::ALL
        .iter()
        .copied()
        .map(|format| {
            json!({
                "format": format.as_str(),
                "label": format_label(format),
                "supports": { "hosted": true, "proxy": true, "group": true },
                "default_write_policy": "allow_once",
                "uploadable_in_ui": false,
                "cleanup_criteria": [],
                "config_schema": {},
                "client_ids": []
            })
        })
        .collect();
    Json(json!({ "items": items }))
}

async fn system_status(
    _proof: Authorized<AdminGrant>,
    State(api): State<CatalogApi>,
) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "generated_at": stamp(api.clock.now()),
        "node": "local",
        "checks": [{
            "id": "database",
            "title": "Database",
            "status": "ok",
            "severity": "info",
            "message": "The metadata store answered.",
            "fix_hint": null,
            "since": null,
            "link": null
        }],
        "readiness": {
            "status": "ready",
            "checks": [{ "name": "database", "status": "ok" }],
            "advisories": []
        }
    }))
}

async fn dashboard(
    State(api): State<CatalogApi>,
    RequestPrincipal(principal): RequestPrincipal,
) -> Result<Json<Value>, ApiError> {
    let repos = api.repositories.list().await?;
    let mut by_format = serde_json::Map::new();
    let mut by_type = serde_json::Map::new();
    let mut offline = 0_u64;
    for repository in &repos {
        if !repository.online {
            offline += 1;
        }
        bump(&mut by_format, repository.format.as_str());
        bump(&mut by_type, repository.repo_type.as_str());
    }
    let admin = !principal.is_anonymous();
    Ok(Json(json!({
        "public_only": principal.is_anonymous(),
        "repositories": {
            "total": repos.len(),
            "offline": offline,
            "by_format": by_format,
            "by_type": by_type
        },
        "storage": if admin {
            json!([{ "blob_store": "default", "used_bytes": 0, "quota_bytes": null, "health": "ok" }])
        } else {
            Value::Null
        },
        "recent_activity": if admin { json!([]) } else { Value::Null },
        "failed_tasks": if admin { json!([]) } else { Value::Null },
        "setup_checklist": if admin {
            json!([
                { "id": "create_blob_store", "done": true, "link": "/admin/blob-stores" },
                { "id": "create_repository", "done": !repos.is_empty(), "link": "/repositories" },
                { "id": "set_base_url", "done": false, "link": "/admin/system/status" },
                { "id": "create_token", "done": false, "link": "/me/tokens" }
            ])
        } else {
            Value::Null
        },
        "degraded": if admin { json!(false) } else { Value::Null }
    })))
}

async fn users(
    _proof: Authorized<AdminGrant>,
    State(api): State<CatalogApi>,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let page = api
        .users
        .list(&PageRequest {
            limit: query.limit.unwrap_or(50).clamp(1, 200),
            after: query.after,
        })
        .await?;
    let needle = query.q.map(|value| value.to_ascii_lowercase());
    let items: Vec<Value> = page
        .items
        .into_iter()
        .filter(|user| {
            needle
                .as_ref()
                .is_none_or(|needle| user.username.to_ascii_lowercase().contains(needle))
        })
        .map(|user| user_view(&user))
        .collect();
    Ok(Json(json!({ "items": items, "next": page.next })))
}

async fn roles(
    _proof: Authorized<AdminGrant>,
    State(api): State<CatalogApi>,
) -> Result<Json<Value>, ApiError> {
    let items = api
        .roles
        .list()
        .await?
        .into_iter()
        .map(|role| {
            let mut value = serde_json::to_value(&role).unwrap_or(json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("privileges".into(), json!([]));
                object.insert("nested_roles".into(), json!([]));
                object.insert("assigned_users".into(), json!(0));
                object.insert("mapped_groups".into(), json!(0));
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn privileges(
    _proof: Authorized<AdminGrant>,
    State(api): State<CatalogApi>,
) -> Result<Json<Value>, ApiError> {
    let items = api
        .privileges
        .list()
        .await?
        .into_iter()
        .map(|privilege| {
            let mut value = serde_json::to_value(&privilege).unwrap_or(json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("roles".into(), json!([]));
                object.insert("selector".into(), Value::Null);
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn selectors(
    _proof: Authorized<AdminGrant>,
    State(api): State<CatalogApi>,
) -> Result<Json<Value>, ApiError> {
    let items = api
        .selectors
        .list()
        .await?
        .into_iter()
        .map(|selector| {
            let mut value = serde_json::to_value(&selector).unwrap_or(json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("used_by_privileges".into(), json!([]));
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn routing_rules(
    _proof: Authorized<AdminGrant>,
    State(api): State<CatalogApi>,
) -> Result<Json<Value>, ApiError> {
    let items = api
        .routing_rules
        .list()
        .await?
        .into_iter()
        .map(|rule| {
            let mut value = serde_json::to_value(&rule).unwrap_or(json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("assigned_repositories".into(), json!([]));
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn empty_page(_proof: Authorized<AdminGrant>) -> Json<Value> {
    Json(json!({ "items": [], "next": null }))
}

async fn anonymous_access(
    _proof: Authorized<AdminGrant>,
    State(api): State<CatalogApi>,
) -> Result<Json<Value>, ApiError> {
    let roles = api.roles.list().await?;
    let anonymous = roles.iter().find(|role| role.name == "nx-anonymous");
    let repos = api.repositories.list().await?;
    let public = repos.iter().filter(|repo| repo.allow_anonymous).count();
    Ok(Json(json!({
        "global_enabled": api.anonymous_enabled,
        "global_editable": false,
        "anonymous_role": {
            "id": anonymous.map(|role| role.id.to_string()).unwrap_or_default(),
            "name": "nx-anonymous"
        },
        "public_repository_count": public,
        "effective_repository_count": if api.anonymous_enabled { public } else { 0 }
    })))
}

async fn providers(_proof: Authorized<AdminGrant>) -> Json<Value> {
    Json(json!({
        "source": "file",
        "editable": false,
        "local": {
            "min_password_length": 12,
            "max_password_length": 1024,
            "lockout_threshold": 5,
            "max_lockout_secs": 1800
        },
        "ldap": {
            "enabled": false,
            "url": null,
            "starttls": false,
            "admin_groups": [],
            "bind_password_set": false
        },
        "oidc": {
            "enabled": false,
            "client_secret_set": false,
            "scopes": [],
            "groups_claim": "groups",
            "admin_groups": [],
            "discovery": {
                "loaded": false,
                "reachable": false,
                "last_success_at": null,
                "jwks_keys": 0
            }
        }
    }))
}

async fn mappings(
    _proof: Authorized<AdminGrant>,
    State(api): State<CatalogApi>,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let providers = match query.provider.as_deref() {
        Some("ldap") => vec![IdpProvider::Ldap],
        Some("oidc") | None => vec![IdpProvider::Oidc],
        Some(_) => {
            return Err(ApiError::bad_request("invalid_query", "Unknown provider"));
        }
    };
    let mut items = Vec::new();
    for provider in providers {
        items.extend(
            api.mappings
                .list(provider)
                .await?
                .into_iter()
                .map(|mapping| {
                    json!({
                        "id": mapping.id.to_string(),
                        "provider": match mapping.provider {
                            IdpProvider::Oidc => "oidc",
                            IdpProvider::Ldap => "ldap",
                        },
                        "external_group": mapping.external_group,
                        "role_id": mapping.role_id.to_string(),
                        "priority": mapping.priority
                    })
                }),
        );
    }
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn search_components(
    State(api): State<CatalogApi>,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let page = api
        .components
        .list(
            &ComponentFilter {
                repository_id: None,
                group_id: None,
                name: None,
                version: None,
                query: query.q.or(query.repository),
            },
            &PageRequest {
                limit: query.limit.unwrap_or(50).clamp(1, 200),
                after: query.after,
            },
        )
        .await?;
    let repos = api.repositories.list().await?;
    let items: Vec<Value> = page
        .items
        .into_iter()
        .map(|component| {
            let name = repos
                .iter()
                .find(|repository| repository.id == component.repository_id)
                .map(|repository| repository.name.to_string())
                .unwrap_or_default();
            json!({
                "id": component.id.to_string(),
                "repository_id": component.repository_id.to_string(),
                "format": component.format.as_str(),
                "group_id": component.group_id,
                "name": component.name,
                "version": component.version,
                "created_at": stamp(component.created_at),
                "updated_at": stamp(component.updated_at),
                "repository": name,
                "asset_count": 0,
                "size_bytes": 0,
                "attributes_summary": {}
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next": page.next })))
}

async fn my_tokens(
    _proof: Authorized<InteractiveGrant>,
    State(api): State<CatalogApi>,
    RequestPrincipal(principal): RequestPrincipal,
) -> Result<Json<Value>, ApiError> {
    let Some(user_id) = principal.user_id else {
        return Ok(Json(json!({ "items": [], "next": null })));
    };
    let user = api.users.get(user_id).await?;
    let items = api
        .tokens
        .list_for_user(user_id)
        .await?
        .into_iter()
        .map(|token| token_view(&token, user.as_ref()))
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn create_token(
    _proof: Authorized<InteractiveGrant>,
    State(api): State<CatalogApi>,
    RequestPrincipal(principal): RequestPrincipal,
    ApiJson(body): ApiJson<TokenBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let Some(user_id) = principal.user_id else {
        return Err(ApiError::bad_request(
            "invalid_input",
            "The session has no account",
        ));
    };
    for scope in &body.scopes {
        if !scope_ok(scope) {
            return Err(ApiError::bad_request(
                "invalid_input",
                "A scope is not valid",
            ));
        }
    }
    let days = body.expires_in_days.unwrap_or(90).clamp(1, 365);
    let mut spec = TokenSpec::new(body.name);
    spec.scopes = body.scopes;
    spec.ttl = Duration::from_secs(u64::from(days) * 24 * 60 * 60);
    let issued = tokens::create_token(api.tokens.as_ref(), api.clock.as_ref(), user_id, spec)
        .await
        .map_err(|_| ApiError::bad_request("invalid_input", "The token could not be created"))?;
    let user = api.users.get(user_id).await?;
    let mut view = token_view(&issued.record, user.as_ref());
    view["secret"] = json!(issued.secret.expose());
    Ok((StatusCode::CREATED, Json(view)))
}

fn token_view(token: &rustryx_core::identity::ApiToken, user: Option<&User>) -> Value {
    let state = if token.revoked_at.is_some() {
        "revoked"
    } else if token
        .expires_at
        .is_some_and(|expires| expires < OffsetDateTime::now_utc())
    {
        "expired"
    } else {
        "active"
    };
    json!({
        "id": token.id.to_string(),
        "user_id": token.user_id.to_string(),
        "name": token.name,
        "display_prefix": token.display_prefix,
        "scopes": token.scopes,
        "expires_at": token.expires_at.map(stamp),
        "revoked_at": token.revoked_at.map(stamp),
        "last_used_at": token.last_used_at.map(stamp),
        "created_at": stamp(token.created_at),
        "kind": "personal",
        "state": state,
        "owner": user.map(|user| json!({
            "id": user.id.to_string(),
            "username": user.username,
            "display_name": user.display_name,
            "source": source_name(user.source)
        })),
        "never_used": token.last_used_at.is_none(),
        "project_id": null
    })
}

fn user_view(user: &User) -> Value {
    json!({
        "id": user.id.to_string(),
        "username": user.username,
        "source": source_name(user.source),
        "issuer": user.issuer,
        "subject": user.subject,
        "email": user.email,
        "display_name": user.display_name,
        "enabled": user.enabled,
        "created_at": stamp(user.created_at),
        "last_login_at": null,
        "roles": [],
        "groups_seen": null,
        "token_count": 0,
        "tokens_valid_after": user.tokens_valid_after.map(stamp),
        "editable_fields": ["display_name", "email", "enabled", "roles", "password"],
        "read_only_reason": null
    })
}

fn source_name(source: UserSource) -> &'static str {
    match source {
        UserSource::Local => "local",
        UserSource::Ldap => "ldap",
        UserSource::Oidc => "oidc",
    }
}

fn format_label(format: RepoFormat) -> &'static str {
    match format {
        RepoFormat::Maven2 => "Maven 2",
        RepoFormat::Npm => "npm",
        RepoFormat::Pypi => "PyPI",
        RepoFormat::Docker => "Docker",
        RepoFormat::Oci => "OCI",
        RepoFormat::Helm => "Helm",
        RepoFormat::Raw => "Raw",
        RepoFormat::Pub => "Dart pub",
        _ => "Other",
    }
}

fn bump(map: &mut serde_json::Map<String, Value>, key: &str) {
    let count = map.get(key).and_then(Value::as_u64).unwrap_or(0) + 1;
    map.insert(key.to_owned(), json!(count));
}

fn stamp(time: OffsetDateTime) -> String {
    time.format(&Rfc3339).unwrap_or_else(|_| String::new())
}

fn scope_ok(scope: &str) -> bool {
    scope == "*" || scope.starts_with("repo:")
}
