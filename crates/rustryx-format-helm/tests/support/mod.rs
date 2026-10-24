//! Engine harness for the helm handler tests.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unused_self,
    reason = "tests: a panic is the failure signal"
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response};
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
use rustryx_format_helm::HelmHandler;
use rustryx_testkit::{FixedClock, MemAuditSink, MemBlobStore, MemStore, fixtures};
use time::OffsetDateTime;
use url::Url;

#[derive(Default)]
pub struct FakeRbac {
    grants: Mutex<HashSet<(String, String, Action)>>,
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
}

#[async_trait]
impl RbacChecker for FakeRbac {
    async fn check(
        &self,
        principal: &Principal,
        action: Action,
        resource: &ResourceRef,
    ) -> Result<Decision, AuthError> {
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
        Ok(self
            .grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(user, repo, _)| user == &principal.username && repo == repository))
    }
}

pub struct SingleStore(pub Arc<dyn BlobStore>);

#[async_trait]
impl BlobStoreRegistry for SingleStore {
    async fn store_by_id(&self, _id: BlobStoreId) -> Result<Arc<dyn BlobStore>, BlobError> {
        Ok(Arc::clone(&self.0))
    }
}

fn fast_executor() -> ExecutorConfig {
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

pub struct World {
    pub upstream: Arc<FakeUpstream>,
    pub rbac: Arc<FakeRbac>,
    pub engine: FormatEngine,
    store: MemStore,
}

impl World {
    pub fn new() -> Self {
        Self::assemble(HelmHandler::new())
    }

    pub fn with_handler(handler: HelmHandler) -> Self {
        Self::assemble(handler)
    }

    fn assemble(handler: HelmHandler) -> Self {
        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(handler)).unwrap();
        let blobs = MemBlobStore::new();
        let clock = Arc::new(FixedClock::new(
            OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        ));
        let store = MemStore::with_builtin_roles()
            .with_clock(Arc::clone(&clock) as Arc<dyn rustryx_core::ports::Clock>)
            .with_default_blob_store();
        let audit = MemAuditSink::new();
        let rbac = Arc::new(FakeRbac::default());
        let ports = Ports::from_store(
            Arc::new(store.clone()),
            rbac.clone(),
            Arc::new(audit),
            clock,
            Arc::new(SingleStore(Arc::new(blobs))),
        );
        let upstream = FakeUpstream::new();
        let engine = FormatEngine::builder(ports, registry)
            .limits(Limits::default())
            .executor_config(fast_executor())
            .url_policy(UpstreamUrlPolicy::any_public())
            .upstream_client(upstream.clone())
            .build()
            .unwrap();
        Self {
            upstream,
            rbac,
            engine,
            store,
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
            RepoFormat::Helm,
            RepoType::Hosted,
        ))
        .await
    }

    pub async fn hosted_with(&self, name: &str, policy: WritePolicy) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Helm, RepoType::Hosted);
        repository.write_policy = policy;
        self.add(repository).await
    }

    pub async fn proxy(&self, name: &str) -> Arc<Repository> {
        self.add(fixtures::repository(
            name,
            RepoFormat::Helm,
            RepoType::Proxy,
        ))
        .await
    }

    pub async fn group(&self, name: &str, members: &[&Arc<Repository>]) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Helm, RepoType::Group);
        repository.group = Some(GroupConfig {
            members: members.iter().map(|member| member.name.clone()).collect(),
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

    pub async fn get(&self, repo: &Arc<Repository>, path: &str, who: &Principal) -> Response<Body> {
        self.send(repo, Method::GET, path, who, Body::empty()).await
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
        data: impl Into<Bytes>,
    ) -> Response<Body> {
        let data = data.into();
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, HeaderValue::from(data.len()));
        let head = self.head(Method::PUT, path, who).with_headers(headers);
        self.engine
            .serve(Arc::clone(repo), head, Body::from(data))
            .await
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

    pub fn admin(&self, repositories: &[&Arc<Repository>]) -> Principal {
        for repository in repositories {
            self.rbac.grant_all("admin", repository.name.as_str());
        }
        principal("admin")
    }

    pub fn grant_read(&self, user: &str, repositories: &[&Arc<Repository>]) {
        for repository in repositories {
            self.rbac.grant_read(user, repository.name.as_str());
        }
    }

    pub fn grant_all(&self, user: &str, repositories: &[&Arc<Repository>]) {
        for repository in repositories {
            self.rbac.grant_all(user, repository.name.as_str());
        }
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

pub async fn body_bytes(response: Response<Body>) -> Bytes {
    rustryx_format_core::stream::read_bounded(response.into_body(), u64::MAX)
        .await
        .unwrap_or_default()
}

pub fn header_of<'a>(response: &'a Response<Body>, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}
