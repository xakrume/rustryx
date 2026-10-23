//! Shared harness of the integration tests: an in-memory world (metadata store, blob
//! store, scripted upstream, fake authorizer) and `RawLikeHandler`, a trivial format
//! that is nothing but the default flow plus the optional hooks, so that hosted,
//! proxy and group are exercised end to end through the `FormatHandler` contract.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset of the harness"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::duration_suboptimal_units,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::similar_names,
    clippy::match_wild_err_arm,
    clippy::manual_assert,
    clippy::semicolon_if_nothing_returned,
    clippy::match_single_binding,
    clippy::large_futures,
    reason = "test scenarios read best as straight-line code with local helpers"
)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, header};
use rustryx_core::error::{AuthError, BlobError};
use rustryx_core::identity::{Action, AuthMethod, Principal};
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::auth::Coordinate;
use rustryx_core::ports::{BlobStore, Decision, RbacChecker, RepositoryRepo, ResourceRef};
use rustryx_format_core::proxy::breaker::BreakerConfig;
use rustryx_format_core::proxy::executor::{ExecutorConfig, RetryPolicy};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use rustryx_format_core::testing::FakeUpstream;
use rustryx_format_core::{
    ArtifactRequest, BlobStoreRegistry, ComponentSpec, ContentEvent, ContentEvents, ContentKind,
    ErrorInfo, FormatContext, FormatEngine, FormatError, FormatHandler, FormatRegistry,
    GroupIndexMerger, GroupNameClaimer, Limits, MergeInput, MergedDocument, NameClaim, Ports,
    ProxyFetch, RequestHead, UploadSpec, classify_path_addressed,
};
use rustryx_testkit::fixtures;
use rustryx_testkit::{FixedClock, MemAuditSink, MemBlobStore, MemStore};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use url::Url;

/// A scripted authorizer: explicit grants, a switch that makes it fail, and a log of
/// every check so tests can assert what was (not) asked.
#[derive(Default)]
pub struct FakeRbac {
    grants: Mutex<HashSet<(String, String, Action)>>,
    fail: AtomicBool,
    failing_repositories: Mutex<HashSet<String>>,
    checks: Mutex<Vec<(String, String, Action, String)>>,
}

impl FakeRbac {
    /// Grants `actions` on `repository` to `user`.
    pub fn grant(&self, user: &str, repository: &str, actions: &[Action]) {
        let mut grants = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        for action in actions {
            grants.insert((user.to_owned(), repository.to_owned(), *action));
        }
    }

    /// Grants read and browse on `repository` to `user`.
    pub fn grant_read(&self, user: &str, repository: &str) {
        self.grant(user, repository, &[Action::Read, Action::Browse]);
    }

    /// Grants everything on `repository` to `user`.
    pub fn grant_all(&self, user: &str, repository: &str) {
        self.grant(
            user,
            repository,
            &[Action::Browse, Action::Read, Action::Write, Action::Delete],
        );
    }

    /// Makes checks for one repository fail, the others keep working.
    pub fn fail_repository(&self, repository: &str) {
        self.failing_repositories
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(repository.to_owned());
    }

    /// Makes every check fail like an unreachable group provider.
    pub fn set_failing(&self, failing: bool) {
        self.fail.store(failing, Ordering::SeqCst);
    }

    /// `(user, repository, action, path)` of every check so far.
    pub fn checks(&self) -> Vec<(String, String, Action, String)> {
        self.checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl RbacChecker for FakeRbac {
    async fn check(
        &self,
        principal: &Principal,
        action: Action,
        resource: &ResourceRef,
    ) -> Result<Decision, AuthError> {
        self.checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((
                principal.username.clone(),
                resource.repository.clone(),
                action,
                resource.path.clone(),
            ));
        if self.fail.load(Ordering::SeqCst)
            || self
                .failing_repositories
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&resource.repository)
        {
            return Err(AuthError::GroupLookupFailed("provider down".into()));
        }
        let allowed = self
            .grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&(
                principal.username.clone(),
                resource.repository.clone(),
                action,
            ));
        Ok(if allowed {
            Decision::Allow
        } else {
            Decision::Deny
        })
    }

    async fn can_see_repository(
        &self,
        principal: &Principal,
        repository: &str,
        _format: RepoFormat,
    ) -> Result<bool, AuthError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(AuthError::GroupLookupFailed("provider down".into()));
        }
        Ok(self
            .grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(user, repo, _)| *user == principal.username && repo == repository))
    }
}

/// Hands the same blob store to every repository.
pub struct SingleStore(pub Arc<dyn BlobStore>);

impl BlobStoreRegistry for SingleStore {
    fn store_for(&self, _repository: &Repository) -> Result<Arc<dyn BlobStore>, BlobError> {
        Ok(Arc::clone(&self.0))
    }
}

/// A registry that has no store, for the failure path.
pub struct NoStore;

impl BlobStoreRegistry for NoStore {
    fn store_for(&self, _repository: &Repository) -> Result<Arc<dyn BlobStore>, BlobError> {
        Err(BlobError::Unavailable("no such blob store".into()))
    }
}

/// Collects content events.
#[derive(Default)]
pub struct RecordingEvents {
    events: Mutex<Vec<ContentEvent>>,
}

impl RecordingEvents {
    pub fn events(&self) -> Vec<ContentEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn orphaned(&self) -> usize {
        self.events()
            .iter()
            .filter(|e| matches!(e, ContentEvent::BlobOrphaned { .. }))
            .count()
    }
}

impl ContentEvents for RecordingEvents {
    fn emit(&self, event: ContentEvent) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }
}

/// Blocks paths that start with a prefix in the named repositories.
pub struct BlockRouting {
    pub blocked: Vec<(String, String)>,
}

#[async_trait]
impl rustryx_format_core::RoutingGate for BlockRouting {
    async fn check(
        &self,
        repository: &Repository,
        request: &ArtifactRequest,
    ) -> Result<rustryx_format_core::RoutingDecision, FormatError> {
        let blocked = self.blocked.iter().any(|(repo, prefix)| {
            repo == repository.name.as_str() && request.path.as_str().starts_with(prefix.as_str())
        });
        Ok(if blocked {
            rustryx_format_core::RoutingDecision::Block
        } else {
            rustryx_format_core::RoutingDecision::Allow
        })
    }
}

/// A routing gate that cannot decide.
pub struct FailingRouting;

#[async_trait]
impl rustryx_format_core::RoutingGate for FailingRouting {
    async fn check(
        &self,
        _repository: &Repository,
        _request: &ArtifactRequest,
    ) -> Result<rustryx_format_core::RoutingDecision, FormatError> {
        Err(FormatError::internal("routing rules unavailable"))
    }
}

/// An audit sink that always fails.
pub struct FailingAudit;

#[async_trait]
impl rustryx_core::ports::AuditSink for FailingAudit {
    async fn record(
        &self,
        _event: rustryx_core::audit::AuditEvent,
    ) -> Result<(), rustryx_core::error::StoreError> {
        Err(rustryx_core::error::StoreError::Unavailable(
            "audit down".into(),
        ))
    }
}

/// Union of lines, in group order, without duplicates. The merge of `RawLikeHandler`.
pub struct LineMerger;

impl GroupIndexMerger for LineMerger {
    fn merge(&self, input: MergeInput<'_>) -> Result<MergedDocument, FormatError> {
        let mut seen = HashSet::new();
        let mut lines = Vec::new();
        for document in &input.documents {
            let text = std::str::from_utf8(&document.body)
                .map_err(|_| FormatError::unprocessable("member document is not utf-8"))?;
            for line in text.lines() {
                if seen.insert(line.to_owned()) {
                    lines.push(line.to_owned());
                }
            }
        }
        Ok(MergedDocument {
            content_type: "text/plain".into(),
            body: Bytes::from(lines.join("\n")),
        })
    }
}

/// The first path segment is the "package name".
pub struct FirstSegmentClaimer;

impl GroupNameClaimer for FirstSegmentClaimer {
    fn claim_for(&self, request: &ArtifactRequest) -> Option<NameClaim> {
        request
            .path
            .segments()
            .next()
            .map(|name| NameClaim::package(name.to_owned()))
    }
}

/// The smallest possible format. `*.index` paths are index documents.
pub struct RawLikeHandler;

#[async_trait]
impl FormatHandler for RawLikeHandler {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Raw]
    }

    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        let mut request = classify_path_addressed(repository, head)?;
        let first = request.path.segments().next().map(str::to_owned);
        if let Some(name) = first {
            request = request.with_coordinate(Coordinate {
                name: Some(name),
                ..Coordinate::default()
            });
        }
        if request.path.as_str().ends_with(".index") {
            request = request.index();
        }
        Ok(request)
    }

    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        match (cx.repository().repo_type, req.action, req.kind) {
            (RepoType::Hosted, Action::Write, _) => {
                let mut spec = UploadSpec::from_request(&req);
                spec.component = req.path.segments().next().map(|name| ComponentSpec {
                    group: None,
                    name: name.to_owned(),
                    version: None,
                });
                let outcome = cx.hosted().upload(spec, body).await?;
                let mut response = Response::new(Body::empty());
                *response.status_mut() = if outcome.created() {
                    http::StatusCode::CREATED
                } else {
                    http::StatusCode::OK
                };
                Ok(response)
            }
            (RepoType::Hosted, Action::Read, _) if req.path.as_str() == "__whoami" => {
                Ok(Response::new(Body::from(format!(
                    "origin={} repo={}",
                    cx.origin_repository().name,
                    cx.repository().name
                ))))
            }
            (RepoType::Proxy, Action::Read, ContentKind::Index) => {
                let document = cx.proxy().fetch_document(ProxyFetch::new(&req)).await?;
                let mut response = Response::new(Body::from(document.body.clone()));
                response.headers_mut().extend(document.response_headers());
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                Ok(response)
            }
            _ => cx.serve_default(req, body).await,
        }
    }

    fn group_merge(&self) -> Option<&dyn GroupIndexMerger> {
        Some(&LineMerger)
    }

    fn group_claims(&self) -> Option<&dyn GroupNameClaimer> {
        Some(&FirstSegmentClaimer)
    }

    fn render_error(&self, error: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
        // A protocol-shaped body, to prove the hook replaces the generic envelope.
        let mut response = Response::new(Body::from(format!("RAW-ERROR {}", error.code())));
        *response.status_mut() = error.status();
        response.headers_mut().extend(error.headers());
        if let Some(id) = info.request_id
            && let Ok(value) = HeaderValue::from_str(id)
        {
            response.headers_mut().insert("x-request-id", value);
        }
        response
    }
}

/// An authenticated principal named `name`.
pub fn principal(name: &str) -> Principal {
    let mut principal = fixtures::principal_with_roles(&[]);
    name.clone_into(&mut principal.username);
    principal.auth_method = AuthMethod::BasicLocal;
    principal
}

/// The anonymous principal.
pub fn anonymous() -> Principal {
    Principal::anonymous()
}

/// Fast retries so failure tests do not sleep.
pub fn fast_executor() -> ExecutorConfig {
    ExecutorConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            jitter: false,
        },
        permit_wait: Duration::from_millis(300),
        idle_timeout: Duration::from_millis(250),
        metadata_timeout: Duration::from_millis(600),
        breaker: BreakerConfig {
            min_requests: 4,
            ..BreakerConfig::default()
        },
    }
}

/// How a test world reaches its upstream.
pub enum Transport {
    /// The reqwest client with this policy and client config.
    Real(
        UpstreamUrlPolicy,
        rustryx_format_core::proxy::upstream::ReqwestClientConfig,
    ),
    /// A client supplied by the test.
    Custom(Arc<dyn rustryx_format_core::proxy::upstream::UpstreamClient>),
}

/// Everything a test needs.
pub struct World {
    pub store: MemStore,
    pub blobs: MemBlobStore,
    pub audit: MemAuditSink,
    pub rbac: Arc<FakeRbac>,
    pub upstream: Arc<FakeUpstream>,
    pub clock: Arc<FixedClock>,
    pub events: Arc<RecordingEvents>,
    pub shutdown: CancellationToken,
    pub engine: FormatEngine,
}

impl World {
    pub fn new() -> Self {
        Self::assemble(
            Limits::default(),
            MemBlobStore::new(),
            None,
            fast_executor(),
        )
    }

    pub fn with(limits: Limits) -> Self {
        Self::assemble(limits, MemBlobStore::new(), None, fast_executor())
    }

    pub fn with_blob_store(blobs: MemBlobStore) -> Self {
        Self::assemble(Limits::default(), blobs, None, fast_executor())
    }

    /// A world whose blob store registry cannot provide any store.
    pub fn without_blob_store() -> Self {
        Self::assemble(
            Limits::default(),
            MemBlobStore::new(),
            Some(Arc::new(NoStore)),
            fast_executor(),
        )
    }

    /// A world whose ports are adjusted by `customize` before the engine is built.
    pub fn with_ports(customize: impl FnOnce(&mut Ports)) -> Self {
        Self::assemble_with(
            Limits::default(),
            MemBlobStore::new(),
            None,
            fast_executor(),
            customize,
        )
    }

    /// A world that talks to real HTTP servers through the reqwest client, with the
    /// given URL policy and client settings.
    pub fn real_http(
        policy: UpstreamUrlPolicy,
        client_config: rustryx_format_core::proxy::upstream::ReqwestClientConfig,
    ) -> Self {
        Self::assemble_full(
            Limits::default(),
            MemBlobStore::new(),
            None,
            fast_executor(),
            |_| {},
            Some(Transport::Real(policy, client_config)),
        )
    }

    /// A world whose upstream is `client` (for failure injection the scripted fake cannot do).
    pub fn with_client(
        client: Arc<dyn rustryx_format_core::proxy::upstream::UpstreamClient>,
    ) -> Self {
        Self::assemble_full(
            Limits::default(),
            MemBlobStore::new(),
            None,
            fast_executor(),
            |_| {},
            Some(Transport::Custom(client)),
        )
    }

    /// A world with custom executor settings (timeouts, retries, breaker).
    pub fn with_executor(executor: ExecutorConfig) -> Self {
        Self::assemble(Limits::default(), MemBlobStore::new(), None, executor)
    }

    /// A world with custom limits and executor settings.
    pub fn with_limits_and_executor(limits: Limits, executor: ExecutorConfig) -> Self {
        Self::assemble(limits, MemBlobStore::new(), None, executor)
    }

    fn assemble(
        limits: Limits,
        blobs: MemBlobStore,
        stores: Option<Arc<dyn BlobStoreRegistry>>,
        executor: ExecutorConfig,
    ) -> Self {
        Self::assemble_with(limits, blobs, stores, executor, |_| {})
    }

    fn assemble_with(
        limits: Limits,
        blobs: MemBlobStore,
        stores: Option<Arc<dyn BlobStoreRegistry>>,
        executor: ExecutorConfig,
        customize: impl FnOnce(&mut Ports),
    ) -> Self {
        Self::assemble_full(limits, blobs, stores, executor, customize, None)
    }

    /// `real_http`: use the reqwest client with this policy and client config instead of
    /// the scripted upstream.
    fn assemble_full(
        limits: Limits,
        blobs: MemBlobStore,
        stores: Option<Arc<dyn BlobStoreRegistry>>,
        executor: ExecutorConfig,
        customize: impl FnOnce(&mut Ports),
        transport: Option<Transport>,
    ) -> Self {
        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(RawLikeHandler)).unwrap();
        let stores: Arc<dyn BlobStoreRegistry> =
            stores.unwrap_or_else(|| Arc::new(SingleStore(Arc::new(blobs.clone()))));
        let clock = Arc::new(FixedClock::new(
            OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        ));
        let store = MemStore::with_builtin_roles()
            .with_clock(clock.clone())
            .with_default_blob_store();
        let audit = MemAuditSink::new();
        let rbac = Arc::new(FakeRbac::default());
        let events = Arc::new(RecordingEvents::default());
        let mut ports = Ports::from_store(
            Arc::new(store.clone()),
            rbac.clone(),
            Arc::new(audit.clone()),
            clock.clone(),
            stores,
        );
        ports.events = events.clone();
        customize(&mut ports);
        let upstream = FakeUpstream::new();
        let shutdown = CancellationToken::new();
        let builder = FormatEngine::builder(ports, registry)
            .limits(limits)
            .executor_config(executor)
            .shutdown_token(shutdown.clone());
        let builder = match transport {
            Some(Transport::Real(policy, client_config)) => {
                builder.url_policy(policy).client_config(client_config)
            }
            Some(Transport::Custom(client)) => builder
                .url_policy(UpstreamUrlPolicy::any_public())
                .upstream_client(client),
            None => builder
                .url_policy(UpstreamUrlPolicy::any_public())
                .upstream_client(upstream.clone()),
        };
        let engine = builder.build().unwrap();
        Self {
            store,
            blobs,
            audit,
            rbac,
            upstream,
            clock,
            events,
            shutdown,
            engine,
        }
    }

    /// Creates a repository in the store and returns it.
    pub async fn add(&self, repository: Repository) -> Arc<Repository> {
        RepositoryRepo::create(&self.store, &repository)
            .await
            .unwrap();
        Arc::new(repository)
    }

    pub async fn hosted(&self, name: &str) -> Arc<Repository> {
        self.add(fixtures::repository(
            name,
            RepoFormat::Raw,
            RepoType::Hosted,
        ))
        .await
    }

    pub async fn proxy(&self, name: &str) -> Arc<Repository> {
        self.add(fixtures::repository(name, RepoFormat::Raw, RepoType::Proxy))
            .await
    }

    /// A proxy repository with its own remote url.
    pub async fn proxy_at(&self, name: &str, remote: &str) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Raw, RepoType::Proxy);
        remote.clone_into(&mut repository.proxy.as_mut().unwrap().remote_url);
        self.add(repository).await
    }

    pub async fn group(&self, name: &str, members: &[&Arc<Repository>]) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Raw, RepoType::Group);
        repository.group = Some(rustryx_core::model::GroupConfig {
            members: members.iter().map(|m| m.name.clone()).collect(),
        });
        self.add(repository).await
    }

    /// Builds a head for `who`.
    pub fn head(&self, method: Method, path: &str, who: &Principal) -> RequestHead {
        RequestHead::new(
            method,
            path,
            who.clone(),
            Url::parse("https://rustryx.test").unwrap(),
        )
    }

    /// Sends a request through the engine.
    pub async fn send(
        &self,
        repository: &Arc<Repository>,
        method: Method,
        path: &str,
        who: &Principal,
        body: Body,
    ) -> Response<Body> {
        let head = self.head(method, path, who);
        self.engine.serve(Arc::clone(repository), head, body).await
    }

    pub async fn send_with(
        &self,
        repository: &Arc<Repository>,
        head: RequestHead,
        body: Body,
    ) -> Response<Body> {
        self.engine.serve(Arc::clone(repository), head, body).await
    }

    /// Uploads as a throwaway administrator and asserts success.
    pub async fn put_as_admin(
        &self,
        repository: &Arc<Repository>,
        path: &str,
        data: &'static [u8],
    ) {
        self.rbac.grant_all("admin", repository.name.as_str());
        let response = self.put(repository, path, &principal("admin"), data).await;
        assert!(
            response.status().is_success(),
            "admin upload failed: {}",
            response.status()
        );
    }

    pub async fn get(
        &self,
        repository: &Arc<Repository>,
        path: &str,
        who: &Principal,
    ) -> Response<Body> {
        self.send(repository, Method::GET, path, who, Body::empty())
            .await
    }

    pub async fn put(
        &self,
        repository: &Arc<Repository>,
        path: &str,
        who: &Principal,
        data: &'static [u8],
    ) -> Response<Body> {
        self.send(repository, Method::PUT, path, who, Body::from(data))
            .await
    }

    /// Gives `user` full rights on the repository.
    pub fn allow_all(&self, user: &str, repositories: &[&Arc<Repository>]) {
        for repository in repositories {
            self.rbac.grant_all(user, repository.name.as_str());
        }
    }

    /// Waits until the engine has no background task and no fill in flight.
    pub async fn settle(&self) {
        for _ in 0..400 {
            if self.engine.fills_in_flight() == 0 && self.engine.running_tasks() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "engine did not settle: {} fills, {} tasks",
            self.engine.fills_in_flight(),
            self.engine.running_tasks()
        );
    }
}

/// Reads a whole response body (tests only).
pub async fn body_bytes(response: Response<Body>) -> Bytes {
    rustryx_format_core::stream::read_bounded(response.into_body(), u64::MAX)
        .await
        .unwrap_or_default()
}

pub async fn body_text(response: Response<Body>) -> String {
    String::from_utf8_lossy(&body_bytes(response).await).into_owned()
}

/// Header value as text.
pub fn header_of<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Polls until `check` is true, up to two seconds.
pub async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never became true: {what}");
}

/// A request body fed from the test: send chunks, keep the sender alive to stall,
/// drop it to end the body, send an `Err` to break it.
pub fn channel_body() -> (
    tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    Body,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    (tx, Body::from_stream(stream))
}

/// A body that records whether anybody polled it.
pub fn tracked_body(data: &'static [u8]) -> (Arc<AtomicBool>, Body) {
    let polled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&polled);
    let stream = futures::stream::unfold(Some(data), move |state| {
        let flag = Arc::clone(&flag);
        async move {
            flag.store(true, Ordering::SeqCst);
            state.map(|data| (Ok::<_, std::io::Error>(Bytes::from_static(data)), None))
        }
    });
    (polled, Body::from_stream(stream))
}

/// A `BlobStore` that delegates to a `MemBlobStore` and can misbehave on demand.
pub struct FlakyStore {
    pub inner: MemBlobStore,
    shared: Arc<FlakyShared>,
}

#[derive(Default)]
struct FlakyShared {
    after_commit: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    fail_delete: AtomicBool,
    fail_commit: AtomicBool,
}

impl FlakyStore {
    pub fn new(inner: MemBlobStore) -> Self {
        Self {
            inner,
            shared: Arc::new(FlakyShared::default()),
        }
    }

    /// Runs `hook` right after a successful commit, before the caller sees the result.
    pub fn after_commit(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .after_commit
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(hook));
    }

    pub fn fail_delete(&self, fail: bool) {
        self.shared.fail_delete.store(fail, Ordering::SeqCst);
    }

    pub fn fail_commit(&self, fail: bool) {
        self.shared.fail_commit.store(fail, Ordering::SeqCst);
    }
}

struct FlakyWriter {
    inner: Box<dyn rustryx_core::ports::BlobWriter>,
    shared: Arc<FlakyShared>,
}

#[async_trait]
impl rustryx_core::ports::BlobWriter for FlakyWriter {
    async fn write(&mut self, chunk: Bytes) -> Result<(), BlobError> {
        self.inner.write(chunk).await
    }

    async fn commit(self: Box<Self>) -> Result<rustryx_core::ports::BlobCommit, BlobError> {
        if self.shared.fail_commit.load(Ordering::SeqCst) {
            let _ = self.inner.abort().await;
            return Err(BlobError::Unavailable("commit refused".into()));
        }
        let shared = Arc::clone(&self.shared);
        let result = self.inner.commit().await;
        if result.is_ok()
            && let Some(hook) = shared
                .after_commit
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
        {
            hook();
        }
        result
    }

    async fn abort(self: Box<Self>) -> Result<(), BlobError> {
        self.inner.abort().await
    }
}

#[async_trait]
impl BlobStore for FlakyStore {
    fn kind(&self) -> rustryx_core::model::BlobStoreKind {
        self.inner.kind()
    }

    async fn begin(
        &self,
        key: &rustryx_core::ports::BlobKey,
        size_hint: Option<u64>,
    ) -> Result<Box<dyn rustryx_core::ports::BlobWriter>, BlobError> {
        let inner = self.inner.begin(key, size_hint).await?;
        Ok(Box::new(FlakyWriter {
            inner,
            shared: Arc::clone(&self.shared),
        }))
    }

    async fn get(
        &self,
        key: &rustryx_core::ports::BlobKey,
        range: Option<rustryx_core::ports::ByteRange>,
    ) -> Result<rustryx_core::ports::BlobRead, BlobError> {
        self.inner.get(key, range).await
    }

    async fn head(
        &self,
        key: &rustryx_core::ports::BlobKey,
    ) -> Result<Option<rustryx_core::ports::BlobMeta>, BlobError> {
        self.inner.head(key).await
    }

    async fn delete(&self, key: &rustryx_core::ports::BlobKey) -> Result<(), BlobError> {
        if self.shared.fail_delete.load(Ordering::SeqCst) {
            return Err(BlobError::Unavailable("delete refused".into()));
        }
        self.inner.delete(key).await
    }

    async fn list_stale_staging(
        &self,
        min_age: Duration,
    ) -> Result<Vec<rustryx_core::ports::BlobKey>, BlobError> {
        self.inner.list_stale_staging(min_age).await
    }

    async fn health(&self) -> Result<(), BlobError> {
        self.inner.health().await
    }
}

impl World {
    /// A world over a [`FlakyStore`] wrapped around a fresh in-memory store.
    pub fn with_flaky_store() -> (Self, Arc<FlakyStore>) {
        let flaky = Arc::new(FlakyStore::new(MemBlobStore::new()));
        let world = Self::assemble(
            Limits::default(),
            flaky.inner.clone(),
            Some(Arc::new(SingleStore(flaky.clone()))),
            fast_executor(),
        );
        (world, flaky)
    }
}
