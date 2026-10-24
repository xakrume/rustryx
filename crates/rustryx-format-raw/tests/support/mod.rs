//! Shared harness of the contract tests: the real `FormatEngine` and the real `RawHandler`
//! over the in-memory ports, a scripted authorizer that knows paths, and a scripted upstream.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset of the harness"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unused_self,
    clippy::similar_names,
    reason = "tests: a panic is the failure signal, and the helpers keep a uniform method shape"
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response};
use rustryx_core::error::{AuthError, BlobError};
use rustryx_core::identity::{Action, AuthMethod, Principal};
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{GroupConfig, RepoFormat, RepoType, Repository, WritePolicy};
use rustryx_core::ports::{BlobStore, Decision, RbacChecker, RepositoryRepo, ResourceRef};
use rustryx_format_core::proxy::breaker::BreakerConfig;
use rustryx_format_core::proxy::executor::{ExecutorConfig, RetryPolicy};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use rustryx_format_core::testing::FakeUpstream;
use rustryx_format_core::{
    BlobStoreRegistry, FormatEngine, FormatRegistry, Limits, Ports, RequestHead,
};
use rustryx_format_raw::RawHandler;
use rustryx_testkit::{FixedClock, MemAuditSink, MemBlobStore, MemStore, fixtures};
use time::OffsetDateTime;
use url::Url;

/// A scripted authorizer: explicit grants per user and repository, path prefixes that are
/// refused even to a user who has the grant (a content selector), a switch that makes every
/// lookup fail, and a log of every path that was checked.
#[derive(Default)]
pub struct FakeRbac {
    grants: Mutex<HashSet<(String, String, Action)>>,
    denied_prefixes: Mutex<Vec<(String, String)>>,
    denied_exact: Mutex<Vec<(String, String)>>,
    fail: std::sync::atomic::AtomicBool,
    /// `read` lookups fail; `browse` still follows the grants. A listing uses this to prove it
    /// stops on the first failed file check.
    fail_reads: std::sync::atomic::AtomicBool,
    checks: Mutex<Vec<(String, Action, String)>>,
}

impl FakeRbac {
    pub fn grant(&self, user: &str, repository: &str, actions: &[Action]) {
        let mut grants = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        for action in actions {
            grants.insert((user.to_owned(), repository.to_owned(), *action));
        }
    }

    pub fn grant_read(&self, user: &str, repository: &str) {
        self.grant(user, repository, &[Action::Read, Action::Browse]);
    }

    pub fn grant_all(&self, user: &str, repository: &str) {
        self.grant(
            user,
            repository,
            &[Action::Browse, Action::Read, Action::Write, Action::Delete],
        );
    }

    /// Refuses `read` and `browse` of paths that start with `prefix` in `repository`.
    pub fn deny_prefix(&self, repository: &str, prefix: &str) {
        self.denied_prefixes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((repository.to_owned(), prefix.to_owned()));
    }

    /// Refuses `read` and `browse` of exactly `path` in `repository`.
    pub fn deny_exact(&self, repository: &str, path: &str) {
        self.denied_exact
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((repository.to_owned(), path.to_owned()));
    }

    pub fn set_failing(&self, failing: bool) {
        self.fail
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }

    /// Makes `read` lookups fail while `browse` still succeeds.
    pub fn set_fail_reads(&self, failing: bool) {
        self.fail_reads
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }

    /// `(repository, action, path)` of every check so far.
    pub fn checks(&self) -> Vec<(String, Action, String)> {
        self.checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn clear_checks(&self) {
        self.checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
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
            .push((resource.repository.clone(), action, resource.path.clone()));
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AuthError::GroupLookupFailed("provider down".into()));
        }
        if self.fail_reads.load(std::sync::atomic::Ordering::SeqCst) && action == Action::Read {
            return Err(AuthError::GroupLookupFailed("provider down".into()));
        }
        let denied = self
            .denied_prefixes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(repo, prefix)| {
                *repo == resource.repository
                    && matches!(action, Action::Read | Action::Browse)
                    && resource.path.starts_with(prefix.as_str())
            });
        let denied = denied
            || self
                .denied_exact
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .any(|(repo, path)| {
                    *repo == resource.repository
                        && matches!(action, Action::Read | Action::Browse)
                        && resource.path == *path
                });
        let allowed = !denied
            && self
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
        Ok(self
            .grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(user, repo, _)| *user == principal.username && repo == repository))
    }
}

/// Hands the same blob store to every repository and every blob row.
pub struct SingleStore(pub Arc<dyn BlobStore>);

#[async_trait]
impl BlobStoreRegistry for SingleStore {
    async fn store_by_id(&self, _id: BlobStoreId) -> Result<Arc<dyn BlobStore>, BlobError> {
        Ok(Arc::clone(&self.0))
    }
}

pub fn fast_executor() -> ExecutorConfig {
    ExecutorConfig {
        retry: RetryPolicy {
            max_attempts: 2,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            jitter: false,
        },
        permit_wait: Duration::from_millis(300),
        idle_timeout: Duration::from_millis(250),
        metadata_timeout: Duration::from_millis(600),
        breaker: BreakerConfig {
            min_requests: 50,
            ..BreakerConfig::default()
        },
    }
}

pub fn principal(name: &str) -> Principal {
    let mut principal = fixtures::principal_with_roles(&[]);
    name.clone_into(&mut principal.username);
    principal.auth_method = AuthMethod::BasicLocal;
    principal
}

pub fn anonymous() -> Principal {
    Principal::anonymous()
}

/// Everything a test needs.
pub struct World {
    pub store: MemStore,
    pub blobs: MemBlobStore,
    pub audit: MemAuditSink,
    pub rbac: Arc<FakeRbac>,
    pub upstream: Arc<FakeUpstream>,
    pub clock: Arc<FixedClock>,
    pub engine: FormatEngine,
}

impl World {
    pub fn new() -> Self {
        Self::assemble(Limits::default(), RawHandler::new())
    }

    pub fn with_limits(limits: Limits) -> Self {
        Self::assemble(limits, RawHandler::new())
    }

    /// A world whose raw handler is `handler` (a different listing scan bound).
    pub fn with_handler(handler: RawHandler) -> Self {
        Self::assemble(Limits::default(), handler)
    }

    fn assemble(limits: Limits, handler: RawHandler) -> Self {
        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(handler)).unwrap();
        let blobs = MemBlobStore::new();
        let clock = Arc::new(FixedClock::new(
            OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        ));
        let store = MemStore::with_builtin_roles()
            .with_clock(clock.clone())
            .with_default_blob_store();
        let audit = MemAuditSink::new();
        let rbac = Arc::new(FakeRbac::default());
        let ports = Ports::from_store(
            Arc::new(store.clone()),
            rbac.clone(),
            Arc::new(audit.clone()),
            clock.clone(),
            Arc::new(SingleStore(Arc::new(blobs.clone()))),
        );
        let upstream = FakeUpstream::new();
        let engine = FormatEngine::builder(ports, registry)
            .limits(limits)
            .executor_config(fast_executor())
            .url_policy(UpstreamUrlPolicy::any_public())
            .upstream_client(upstream.clone())
            .build()
            .unwrap();
        Self {
            store,
            blobs,
            audit,
            rbac,
            upstream,
            clock,
            engine,
        }
    }

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

    pub async fn hosted_with(&self, name: &str, policy: WritePolicy) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Raw, RepoType::Hosted);
        repository.write_policy = policy;
        self.add(repository).await
    }

    pub async fn hosted_with_config(
        &self,
        name: &str,
        config: serde_json::Value,
    ) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Raw, RepoType::Hosted);
        repository.format_config = config;
        self.add(repository).await
    }

    pub async fn proxy(&self, name: &str) -> Arc<Repository> {
        self.add(fixtures::repository(name, RepoFormat::Raw, RepoType::Proxy))
            .await
    }

    pub async fn group(&self, name: &str, members: &[&Arc<Repository>]) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Raw, RepoType::Group);
        repository.group = Some(GroupConfig {
            members: members.iter().map(|m| m.name.clone()).collect(),
        });
        self.add(repository).await
    }

    pub fn head(&self, method: Method, path: &str, who: &Principal) -> RequestHead {
        RequestHead::new(
            method,
            path,
            who.clone(),
            Url::parse("https://rustryx.test").unwrap(),
        )
    }

    pub async fn send_head(
        &self,
        repository: &Arc<Repository>,
        head: RequestHead,
        body: Body,
    ) -> Response<Body> {
        self.engine.serve(Arc::clone(repository), head, body).await
    }

    pub async fn send(
        &self,
        repository: &Arc<Repository>,
        method: Method,
        path: &str,
        who: &Principal,
        body: Body,
    ) -> Response<Body> {
        let head = self.head(method, path, who);
        self.send_head(repository, head, body).await
    }

    pub async fn get(&self, repo: &Arc<Repository>, path: &str, who: &Principal) -> Response<Body> {
        self.send(repo, Method::GET, path, who, Body::empty()).await
    }

    pub async fn get_with(
        &self,
        repo: &Arc<Repository>,
        path: &str,
        who: &Principal,
        headers: &[(&str, &str)],
    ) -> Response<Body> {
        let head = self
            .head(Method::GET, path, who)
            .with_headers(header_map(headers));
        self.send_head(repo, head, Body::empty()).await
    }

    pub async fn head_of(
        &self,
        repo: &Arc<Repository>,
        path: &str,
        who: &Principal,
    ) -> Response<Body> {
        self.send(repo, Method::HEAD, path, who, Body::empty())
            .await
    }

    pub async fn put(
        &self,
        repo: &Arc<Repository>,
        path: &str,
        who: &Principal,
        data: &'static [u8],
    ) -> Response<Body> {
        self.put_typed(repo, path, who, data, None).await
    }

    pub async fn put_typed(
        &self,
        repo: &Arc<Repository>,
        path: &str,
        who: &Principal,
        data: &'static [u8],
        content_type: Option<&str>,
    ) -> Response<Body> {
        let mut headers = HeaderMap::new();
        if let Some(content_type) = content_type {
            headers.insert(
                http::header::CONTENT_TYPE,
                HeaderValue::from_str(content_type).unwrap(),
            );
        }
        headers.insert(http::header::CONTENT_LENGTH, HeaderValue::from(data.len()));
        let head = self.head(Method::PUT, path, who).with_headers(headers);
        self.send_head(repo, head, Body::from(data)).await
    }

    pub async fn put_with(
        &self,
        repo: &Arc<Repository>,
        path: &str,
        who: &Principal,
        data: &'static [u8],
        headers: &[(&str, &str)],
    ) -> Response<Body> {
        let head = self
            .head(Method::PUT, path, who)
            .with_headers(header_map(headers));
        self.send_head(repo, head, Body::from(data)).await
    }

    pub async fn delete(
        &self,
        repo: &Arc<Repository>,
        path: &str,
        who: &Principal,
    ) -> Response<Body> {
        self.send(repo, Method::DELETE, path, who, Body::empty())
            .await
    }

    /// An administrator with every right on `repositories`.
    pub fn admin(&self, repositories: &[&Arc<Repository>]) -> Principal {
        for repository in repositories {
            self.rbac.grant_all("admin", repository.name.as_str());
        }
        principal("admin")
    }

    pub fn grant_all(&self, user: &str, repositories: &[&Arc<Repository>]) {
        for repository in repositories {
            self.rbac.grant_all(user, repository.name.as_str());
        }
    }

    pub fn grant_read(&self, user: &str, repositories: &[&Arc<Repository>]) {
        for repository in repositories {
            self.rbac.grant_read(user, repository.name.as_str());
        }
    }

    /// Uploads as `admin` and asserts success.
    pub async fn seed(&self, repo: &Arc<Repository>, path: &str, data: &'static [u8]) {
        let admin = self.admin(&[repo]);
        let response = self.put(repo, path, &admin, data).await;
        assert!(
            response.status().is_success(),
            "seed of {path} failed: {}",
            response.status()
        );
    }

    pub async fn settle(&self) {
        for _ in 0..400 {
            if self.engine.fills_in_flight() == 0 && self.engine.running_tasks() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("engine did not settle");
    }
}

pub fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    map
}

pub async fn body_bytes(response: Response<Body>) -> Bytes {
    rustryx_format_core::stream::read_bounded(response.into_body(), u64::MAX)
        .await
        .unwrap_or_default()
}

pub async fn body_text(response: Response<Body>) -> String {
    String::from_utf8_lossy(&body_bytes(response).await).into_owned()
}

pub async fn body_json(response: Response<Body>) -> serde_json::Value {
    serde_json::from_slice(&body_bytes(response).await).expect("a json body")
}

pub fn header_of<'a>(response: &'a Response<Body>, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

/// The names of the entries of a JSON listing, in order, directories with a trailing slash.
pub fn entry_names(listing: &serde_json::Value) -> Vec<String> {
    listing["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| {
            let name = item["name"].as_str().expect("name").to_owned();
            if item["type"] == "directory" {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect()
}
