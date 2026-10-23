//! Shared fixtures for the tests of this crate: stub ports, a route module that
//! exercises every group, and helpers to drive the router in process.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, State};
use axum::http::Request;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode};
use rustryx_core::AuthError;
use rustryx_core::error::{BlobError, DomainError, StoreError};
use rustryx_core::identity::{Action, AuthContext, AuthMethod, Credentials, Principal, RoleRef};
use rustryx_core::ids::RoleId;
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::{Authenticator, Decision, RbacChecker, ResourceRef};
use rustryx_observability::MetricsHandle;
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::{
    ApiError, ApiJson, ApiPath, ApiQuery, ApiSettings, AppState, AuthenticatedPrincipal, Lifecycle,
    MetricsAccess, ProbeReport, ReadinessProbe, RequestPrincipal, RouteModule, RouterBuilder,
};

/// Probe with a configurable answer, delay and call counter.
pub(crate) struct StubProbe {
    report: Mutex<ProbeReport>,
    delay: Mutex<Duration>,
    calls: AtomicUsize,
}

impl StubProbe {
    pub(crate) fn new(report: ProbeReport) -> Arc<Self> {
        Arc::new(Self {
            report: Mutex::new(report),
            delay: Mutex::new(Duration::ZERO),
            calls: AtomicUsize::new(0),
        })
    }

    pub(crate) fn set(&self, report: ProbeReport) {
        *self.report.lock().unwrap_or_else(PoisonError::into_inner) = report;
    }

    pub(crate) fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap_or_else(PoisonError::into_inner) = delay;
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ReadinessProbe for StubProbe {
    async fn check(&self) -> ProbeReport {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let delay = *self.delay.lock().unwrap_or_else(PoisonError::into_inner);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        self.report
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

type AuthFn =
    dyn Fn(&Credentials, &AuthContext) -> Result<Option<Principal>, AuthError> + Send + Sync;

/// Authenticator driven by a closure that records what it was asked.
pub(crate) struct StubAuth {
    decide: Box<AuthFn>,
    seen: Mutex<Vec<(Credentials, AuthContext)>>,
}

impl StubAuth {
    pub(crate) fn new(
        decide: impl Fn(&Credentials, &AuthContext) -> Result<Option<Principal>, AuthError>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            decide: Box::new(decide),
            seen: Mutex::new(Vec::new()),
        })
    }

    /// Always authenticates as this principal.
    pub(crate) fn always(principal: Principal) -> Arc<Self> {
        Self::new(move |_, _| Ok(Some(principal.clone())))
    }

    /// Always fails with a fresh error from `make`.
    pub(crate) fn failing(make: fn() -> AuthError) -> Arc<Self> {
        Self::new(move |_, _| Err(make()))
    }

    pub(crate) fn seen(&self) -> Vec<(Credentials, AuthContext)> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Authenticator for StubAuth {
    async fn authenticate(
        &self,
        credentials: &Credentials,
        context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((credentials.clone(), context.clone()));
        (self.decide)(credentials, context)
    }
}

/// Authorization checker with a fixed answer.
pub(crate) struct StubRbac(pub Result<Decision, fn() -> AuthError>);

#[async_trait]
impl RbacChecker for StubRbac {
    async fn check(
        &self,
        _principal: &Principal,
        _action: Action,
        _resource: &ResourceRef,
    ) -> Result<Decision, AuthError> {
        self.0.map_err(|make| make())
    }

    async fn can_see_repository(
        &self,
        _principal: &Principal,
        _repository: &str,
        _format: RepoFormat,
    ) -> Result<bool, AuthError> {
        Ok(true)
    }
}

pub(crate) fn principal(name: &str, method: AuthMethod, role: &str) -> Principal {
    let mut principal = Principal::anonymous();
    principal.username = name.to_owned();
    principal.auth_method = method;
    principal.roles = vec![RoleRef {
        id: RoleId::generate(),
        name: role.to_owned(),
    }];
    principal
}

pub(crate) fn resource() -> ResourceRef {
    ResourceRef {
        repository: "maven-central".to_owned(),
        format: RepoFormat::Maven2,
        path: "org/example/lib.jar".to_owned(),
        coordinate: None,
    }
}

/// A state that is ready, with metrics and a public `/metrics`.
pub(crate) fn state_with(probe: Arc<StubProbe>) -> AppState {
    state_with_probe_and_settings(probe, |_| {})
}

pub(crate) fn ready_state() -> AppState {
    state_with(StubProbe::new(ProbeReport::healthy()))
}

pub(crate) fn state_with_settings(edit: impl FnOnce(&mut ApiSettings)) -> AppState {
    state_with_probe_and_settings(StubProbe::new(ProbeReport::healthy()), edit)
}

fn state_with_probe_and_settings(
    probe: Arc<StubProbe>,
    edit: impl FnOnce(&mut ApiSettings),
) -> AppState {
    let lifecycle = Lifecycle::new();
    lifecycle.mark_listening();
    let mut settings = ApiSettings {
        metrics_access: MetricsAccess::Public,
        ..ApiSettings::default()
    };
    edit(&mut settings);
    AppState::new(probe)
        .with_lifecycle(lifecycle)
        .with_settings(settings)
        .with_metrics(MetricsHandle::new().expect("metrics"))
}

/// Gate that a handler waits on until the test opens it.
#[derive(Clone, Default)]
pub(crate) struct Gate {
    entered: Arc<Notify>,
    open: Arc<Notify>,
}

impl Gate {
    pub(crate) async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn open(&self) {
        self.open.notify_one();
    }
}

/// Mounts a handful of routes in both groups.
pub(crate) struct TestModule {
    pub gate: Gate,
}

impl TestModule {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            gate: Gate::default(),
        })
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Item {
    name: String,
}

#[derive(serde::Deserialize)]
struct Paging {
    limit: u32,
}

impl RouteModule for TestModule {
    fn name(&self) -> &'static str {
        "test"
    }

    fn mount_api(&self, router: Router<AppState>) -> Router<AppState> {
        let gate = self.gate.clone();
        let gated = Router::new()
            .route("/api/gate", get(gated))
            .with_state(gate);
        router
            .route("/api/whoami", get(whoami))
            .route("/api/me", get(me))
            .route("/api/panic", get(panics))
            .route("/api/slow", get(slow))
            .route("/api/echo", post(echo))
            .route("/api/search", get(search))
            .route("/api/items/{id}", get(item_by_id))
            .route("/api/body", post(read_body))
            .route("/api/large", get(large_json))
            .route("/api/authz", get(authz))
            .route("/api/csp", get(own_csp))
            .route("/api/error/{kind}", get(error_of_kind))
            .route("/api/only-get", get(|| async { "ok" }))
            .merge(gated)
    }

    fn mount_artifacts(&self, router: Router<AppState>) -> Router<AppState> {
        router
            .route("/repository/{name}/{*path}", get(artifact).post(upload))
            .route("/repository-big/{name}", get(large_artifact))
    }
}

async fn whoami(RequestPrincipal(principal): RequestPrincipal) -> String {
    principal.username
}

async fn me(AuthenticatedPrincipal(principal): AuthenticatedPrincipal) -> String {
    principal.username
}

async fn panics() -> &'static str {
    panic!("boom: secret-in-panic-message")
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(3600)).await;
    "late"
}

async fn echo(ApiJson(item): ApiJson<Item>) -> axum::Json<Item> {
    axum::Json(item)
}

async fn search(ApiQuery(paging): ApiQuery<Paging>) -> String {
    paging.limit.to_string()
}

async fn item_by_id(ApiPath(id): ApiPath<u32>) -> String {
    id.to_string()
}

async fn read_body(body: Body) -> Result<String, ApiError> {
    to_bytes(body, usize::MAX)
        .await
        .map(|bytes| bytes.len().to_string())
        .map_err(|e| ApiError::from_body_error(&e))
}

async fn large_json() -> axum::Json<Vec<String>> {
    axum::Json((0..400).map(|i| format!("component-number-{i}")).collect())
}

async fn authz(
    State(state): State<AppState>,
    RequestPrincipal(principal): RequestPrincipal,
) -> Result<&'static str, ApiError> {
    state
        .authorize(&principal, Action::Read, &resource())
        .await?;
    Ok("allowed")
}

async fn own_csp() -> Response {
    ([("content-security-policy", "default-src 'self'")], "page").into_response()
}

async fn error_of_kind(Path(kind): Path<String>) -> ApiError {
    match kind.as_str() {
        "sql" => StoreError::Backend {
            context: "insert repository",
            source: "duplicate key value violates unique constraint \"repositories_name_key\" at host=10.1.2.3".into(),
        }
        .into(),
        "blob-path" => BlobError::Backend {
            context: "write",
            source: "No such file or directory: /var/lib/rustryx/blobs/ab/cd/secret".into(),
        }
        .into(),
        "internal" => DomainError::Internal("stack: /srv/app/src/service.rs:42".to_owned()).into(),
        "unavailable" => {
            DomainError::Unavailable("redis at 10.9.8.7:6379 refused".to_owned()).into()
        }
        "ldap" => AuthError::ProviderUnavailable("ldaps://ldap.corp.internal:636 timeout".to_owned())
            .into(),
        "groups" => AuthError::GroupLookupFailed("ou=groups,dc=corp,dc=internal".to_owned()).into(),
        _ => ApiError::bad_request("unknown_kind", "unknown test error kind"),
    }
}

async fn gated(State(gate): State<Gate>) -> &'static str {
    gate.entered.notify_one();
    gate.open.notified().await;
    "released"
}

async fn artifact(Path((name, path)): Path<(String, String)>) -> String {
    crate::record_repo(&name);
    format!("{name}:{path}")
}

async fn upload(
    Path((_name, _path)): Path<(String, String)>,
    body: Body,
) -> Result<String, ApiError> {
    to_bytes(body, usize::MAX)
        .await
        .map(|bytes| bytes.len().to_string())
        .map_err(|e| ApiError::from_body_error(&e))
}

async fn large_artifact() -> String {
    "x".repeat(8192)
}

/// The router under test: both groups mounted.
pub(crate) fn test_router(state: AppState) -> Router {
    RouterBuilder::new(state).module(TestModule::new()).build()
}

/// A response read to the end.
pub(crate) struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Reply {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("body is not JSON ({e}): {}", self.text()))
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub(crate) fn error_code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
}

pub(crate) async fn call(router: &Router, request: Request<Body>) -> Reply {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router is infallible");
    let (parts, body) = response.into_parts();
    let body = to_bytes(body, usize::MAX).await.expect("body reads");
    Reply {
        status: parts.status,
        headers: parts.headers,
        body,
    }
}

pub(crate) fn request(method: Method, uri: &str) -> http::request::Builder {
    Request::builder().method(method).uri(uri)
}

pub(crate) fn get_request(uri: &str) -> Request<Body> {
    request(Method::GET, uri)
        .body(Body::empty())
        .expect("request")
}

pub(crate) async fn get_uri(router: &Router, uri: &str) -> Reply {
    call(router, get_request(uri)).await
}

/// Collects the JSON log lines of the current thread's test subscriber.
#[derive(Clone, Default)]
pub(crate) struct LogCapture(Arc<Mutex<Vec<u8>>>);

pub(crate) struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.0.clone())
    }
}

impl LogCapture {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }

    pub(crate) fn lines(&self) -> Vec<serde_json::Value> {
        self.text()
            .lines()
            .map(|l| serde_json::from_str(l).expect("log lines are JSON"))
            .collect()
    }

    pub(crate) fn with_msg(&self, msg: &str) -> Vec<serde_json::Value> {
        self.lines()
            .into_iter()
            .filter(|l| l["msg"] == msg)
            .collect()
    }
}

/// Installs a JSON subscriber on this thread until the guard drops.
pub(crate) fn capture_logs(filter: &str) -> (LogCapture, tracing::subscriber::DefaultGuard) {
    let capture = LogCapture::default();
    let options = rustryx_observability::ObservabilityOptions {
        log_filter: filter.to_owned(),
        ..rustryx_observability::ObservabilityOptions::default()
    };
    let subscriber = rustryx_observability::build_subscriber(&options, capture.clone(), None)
        .expect("subscriber");
    (capture, tracing::subscriber::set_default(subscriber))
}
