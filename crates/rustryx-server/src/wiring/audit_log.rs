//! `GET /api/v1/audit-events` for the activity tab and the audit viewer.
//!
//! `repository` matches an event whose target is that repository name, or that name
//! followed by `:` and a path.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use rustryx_api::{AdminGrant, ApiError, ApiQuery, Authorized, Requires, RouteModule, RouteSet};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{AuditQuery, AuditReadRepo, PageRequest};
use serde::Deserialize;
use serde_json::{Value, json};

/// Read side of the audit log.
#[derive(Clone)]
pub struct AuditLogModule {
    audit: Arc<dyn AuditReadRepo>,
}

impl AuditLogModule {
    /// Builds the module.
    #[must_use]
    pub fn new(audit: Arc<dyn AuditReadRepo>) -> Self {
        Self { audit }
    }
}

impl RouteModule for AuditLogModule {
    fn name(&self) -> &'static str {
        "audit-log"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        routes.route(
            "/api/v1/audit-events",
            Requires::Admin,
            axum::routing::get(list_events).with_state(self.audit.clone()),
        )
    }
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    repository: Option<String>,
}

async fn list_events(
    _proof: Authorized<AdminGrant>,
    State(audit): State<Arc<dyn AuditReadRepo>>,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut filter = AuditQuery::new();
    if let Some(name) = query.repository {
        RepoName::parse(&name).map_err(|_| {
            ApiError::bad_request("invalid_input", "The repository name is not valid")
        })?;
        filter.repository = Some(name);
    }
    let page = audit
        .query(
            &filter,
            &PageRequest {
                limit: query.limit.unwrap_or(50).clamp(1, 200),
                after: query.after,
            },
        )
        .await?;
    let items = page
        .items
        .into_iter()
        .map(|stored| {
            let mut value = serde_json::to_value(&stored.event).unwrap_or_else(|_| json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("id".into(), json!(stored.id.to_string()));
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items, "next": page.next })))
}
