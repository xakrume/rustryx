//! `POST /api/v1/blob-stores/{name}/actions/dedup` hardlinks identical files of a local store.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use rustryx_api::{AdminGrant, ApiError, Authorized, Requires, RouteModule, RouteSet};
use rustryx_blob::dedup_directory;
use rustryx_core::model::BlobStoreKind;
use rustryx_core::ports::BlobStoreRepo;
use serde::Serialize;

#[derive(Clone)]
struct DedupApi {
    stores: Arc<dyn BlobStoreRepo>,
    runs: Arc<Mutex<std::collections::HashMap<String, DedupView>>>,
}

/// Routes for local blob-store deduplication.
pub struct DedupModule {
    state: DedupApi,
}

impl DedupModule {
    /// Reads store definitions to find the local directory.
    #[must_use]
    pub fn new(stores: Arc<dyn BlobStoreRepo>) -> Self {
        Self {
            state: DedupApi {
                stores,
                runs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            },
        }
    }
}

impl RouteModule for DedupModule {
    fn name(&self) -> &'static str {
        "blob-dedup"
    }

    fn mount_api(&self, routes: RouteSet) -> RouteSet {
        routes.route(
            "/api/v1/blob-stores/{name}/actions/dedup",
            Requires::Admin,
            axum::routing::get(dedup_status)
                .post(dedup_start)
                .with_state(self.state.clone()),
        )
    }
}

#[derive(Clone, Serialize)]
struct DedupView {
    status: String,
    files: u64,
    hashed: u64,
    links: u64,
    bytes_saved: u64,
    errors: u64,
    error: Option<String>,
}

impl DedupView {
    fn idle() -> Self {
        Self {
            status: "idle".to_owned(),
            files: 0,
            hashed: 0,
            links: 0,
            bytes_saved: 0,
            errors: 0,
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

async fn dedup_status(
    State(api): State<DedupApi>,
    _proof: Authorized<AdminGrant>,
    Path(name): Path<String>,
) -> Json<DedupView> {
    let runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
    Json(runs.get(&name).cloned().unwrap_or_else(DedupView::idle))
}

async fn dedup_start(
    State(api): State<DedupApi>,
    _proof: Authorized<AdminGrant>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let store = api.stores.get_by_name(&name).await?.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "The blob store was not found",
        )
    })?;
    if store.kind != BlobStoreKind::Local {
        return Err(ApiError::bad_request(
            "kind_not_local",
            "Hardlink deduplication applies to a local blob store",
        ));
    }
    let path = store
        .config
        .get("path")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_input",
                "The local blob store has no directory",
            )
        })?;
    {
        let mut runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        if runs.get(&name).is_some_and(|view| view.status == "running") {
            let view = runs.get(&name).cloned().unwrap_or_else(DedupView::running);
            return Ok((StatusCode::OK, Json(view)).into_response());
        }
        runs.insert(name.clone(), DedupView::running());
    }
    let runs = Arc::clone(&api.runs);
    let key = name.clone();
    tokio::task::spawn_blocking(move || {
        let outcome = dedup_directory(std::path::Path::new(&path));
        let mut guard = runs.lock().unwrap_or_else(|poison| poison.into_inner());
        let view = guard.entry(key).or_insert_with(DedupView::running);
        match outcome {
            Ok(report) => {
                view.status = "succeeded".to_owned();
                view.files = report.files;
                view.hashed = report.hashed;
                view.links = report.links;
                view.bytes_saved = report.bytes_saved;
                view.errors = report.errors;
            }
            Err(error) => {
                view.status = "failed".to_owned();
                view.error = Some(error.to_string());
            }
        }
    });
    let view = {
        let runs = api.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        runs.get(&name).cloned().unwrap_or_else(DedupView::running)
    };
    Ok((StatusCode::ACCEPTED, Json(view)).into_response())
}
