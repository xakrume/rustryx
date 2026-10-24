//! Harness of the contract tests: an in-memory world (metadata store, blob store, scripted
//! upstream, scripted authorizer) with the real `FormatEngine` and `MavenHandler`.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset of the harness"
)]
#![allow(
    clippy::unwrap_used,
    clippy::unused_self,
    clippy::format_collect,
    clippy::duration_suboptimal_units,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::large_futures,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    reason = "tests: a panic is the failure signal and scenarios read best as straight-line code"
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, header};
use rustryx_core::error::{AuthError, BlobError};
use rustryx_core::identity::{Action, AuthMethod, Principal};
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{GroupConfig, RepoFormat, RepoType, Repository, WritePolicy};
use rustryx_core::ports::auth::Coordinate;
use rustryx_core::ports::{BlobStore, Decision, RbacChecker, RepositoryRepo, ResourceRef};
use rustryx_format_core::proxy::breaker::BreakerConfig;
use rustryx_format_core::proxy::executor::{ExecutorConfig, RetryPolicy};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use rustryx_format_core::testing::FakeUpstream;
use rustryx_format_core::{
    BlobStoreRegistry, FormatEngine, FormatRegistry, Limits, Ports, RequestHead,
};
use rustryx_format_maven::{MavenConfig, MavenHandler};
use rustryx_testkit::fixtures;
use rustryx_testkit::{FixedClock, MemAuditSink, MemBlobStore, MemStore};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Base of every scripted upstream.
pub const UPSTREAM: &str = "https://repo.upstream.test/maven2";

/// One recorded authorization check.
#[derive(Debug, Clone)]
pub struct Check {
    pub user: String,
    pub repository: String,
    pub action: Action,
    pub path: String,
    pub coordinate: Option<Coordinate>,
}

/// A scripted authorizer: explicit grants, a switch that makes it fail, and a log of every check.
#[derive(Default)]
pub struct FakeRbac {
    grants: Mutex<HashSet<(String, String, Action)>>,
    checks: Mutex<Vec<Check>>,
}

impl FakeRbac {
    pub fn grant(&self, user: &str, repository: &str, actions: &[Action]) {
        let mut grants = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        for action in actions {
            grants.insert((user.to_owned(), repository.to_owned(), *action));
        }
    }

    pub fn grant_all(&self, user: &str, repository: &str) {
        self.grant(
            user,
            repository,
            &[Action::Browse, Action::Read, Action::Write, Action::Delete],
        );
    }

    pub fn grant_read(&self, user: &str, repository: &str) {
        self.grant(user, repository, &[Action::Browse, Action::Read]);
    }

    pub fn checks(&self) -> Vec<Check> {
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
            .push(Check {
                user: principal.username.clone(),
                repository: resource.repository.clone(),
                action,
                path: resource.path.clone(),
                coordinate: resource.coordinate.clone(),
            });
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

/// An authenticated principal named `name`.
pub fn principal(name: &str) -> Principal {
    let mut principal = fixtures::principal_with_roles(&[]);
    name.clone_into(&mut principal.username);
    principal.auth_method = AuthMethod::BasicLocal;
    principal
}

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

/// Everything a test needs.
pub struct World {
    pub store: MemStore,
    pub blobs: MemBlobStore,
    pub audit: MemAuditSink,
    pub rbac: Arc<FakeRbac>,
    pub upstream: Arc<FakeUpstream>,
    pub clock: Arc<FixedClock>,
    pub shutdown: CancellationToken,
    pub engine: FormatEngine,
    pub ports: Ports,
}

impl World {
    pub fn new() -> Self {
        Self::build(MavenConfig::default(), Limits::default())
    }

    pub fn with_config(config: MavenConfig) -> Self {
        Self::build(config, Limits::default())
    }

    pub fn with_limits(limits: Limits) -> Self {
        Self::build(MavenConfig::default(), limits)
    }

    fn build(config: MavenConfig, limits: Limits) -> Self {
        let mut registry = FormatRegistry::new();
        registry
            .register(Arc::new(MavenHandler::with_config(config)))
            .unwrap();
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
        let shutdown = CancellationToken::new();
        let engine = FormatEngine::builder(ports.clone(), registry)
            .limits(limits)
            .executor_config(fast_executor())
            .url_policy(UpstreamUrlPolicy::any_public())
            .upstream_client(upstream.clone())
            .shutdown_token(shutdown.clone())
            .build()
            .unwrap();
        Self {
            store,
            blobs,
            audit,
            rbac,
            upstream,
            clock,
            shutdown,
            engine,
            ports,
        }
    }

    pub async fn add(&self, repository: Repository) -> Arc<Repository> {
        RepositoryRepo::create(&self.store, &repository)
            .await
            .unwrap();
        Arc::new(repository)
    }

    /// A hosted repository with `allow` (overwrite) semantics.
    pub async fn hosted(&self, name: &str) -> Arc<Repository> {
        self.hosted_with(name, WritePolicy::Allow, serde_json::json!({}))
            .await
    }

    pub async fn hosted_with(
        &self,
        name: &str,
        policy: WritePolicy,
        format_config: serde_json::Value,
    ) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Maven2, RepoType::Hosted);
        repository.write_policy = policy;
        repository.format_config = format_config;
        self.add(repository).await
    }

    /// A hosted release repository: `allow_once`, release versions only.
    pub async fn releases(&self, name: &str) -> Arc<Repository> {
        self.hosted_with(
            name,
            WritePolicy::AllowOnce,
            serde_json::json!({"version_policy": "release"}),
        )
        .await
    }

    pub async fn proxy(&self, name: &str) -> Arc<Repository> {
        self.proxy_with(name, serde_json::json!({})).await
    }

    pub async fn proxy_with(
        &self,
        name: &str,
        format_config: serde_json::Value,
    ) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Maven2, RepoType::Proxy);
        UPSTREAM.clone_into(&mut repository.proxy.as_mut().unwrap().remote_url);
        repository.format_config = format_config;
        self.add(repository).await
    }

    pub async fn group(&self, name: &str, members: &[&Arc<Repository>]) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Maven2, RepoType::Group);
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

    pub async fn get(
        &self,
        repository: &Arc<Repository>,
        path: &str,
        who: &Principal,
    ) -> Response<Body> {
        self.send(repository, Method::GET, path, who, Body::empty())
            .await
    }

    pub async fn head_request(
        &self,
        repository: &Arc<Repository>,
        path: &str,
        who: &Principal,
    ) -> Response<Body> {
        self.send(repository, Method::HEAD, path, who, Body::empty())
            .await
    }

    pub async fn put(
        &self,
        repository: &Arc<Repository>,
        path: &str,
        who: &Principal,
        data: impl Into<Bytes>,
    ) -> Response<Body> {
        self.send(repository, Method::PUT, path, who, Body::from(data.into()))
            .await
    }

    pub async fn delete(
        &self,
        repository: &Arc<Repository>,
        path: &str,
        who: &Principal,
    ) -> Response<Body> {
        self.send(repository, Method::DELETE, path, who, Body::empty())
            .await
    }

    /// A user with every right on the repositories.
    pub fn admin(&self, repositories: &[&Arc<Repository>]) -> Principal {
        for repository in repositories {
            self.rbac.grant_all("admin", repository.name.as_str());
        }
        principal("admin")
    }

    /// A user with read rights only.
    pub fn reader(&self, repositories: &[&Arc<Repository>]) -> Principal {
        for repository in repositories {
            self.rbac.grant_read("reader", repository.name.as_str());
        }
        principal("reader")
    }

    /// Deploys the files of a release the way `mvn deploy` does: jar, pom and checksums.
    pub async fn deploy(
        &self,
        repository: &Arc<Repository>,
        who: &Principal,
        group_path: &str,
        artifact: &str,
        version: &str,
    ) {
        for (suffix, body) in [
            (".jar", format!("jar of {artifact} {version}")),
            (".pom", pom(group_path, artifact, version)),
        ] {
            let path = format!("{group_path}/{artifact}/{version}/{artifact}-{version}{suffix}");
            let response = self.put(repository, &path, who, body.clone()).await;
            assert!(
                response.status().is_success(),
                "{path}: {}",
                response.status()
            );
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
        panic!("engine did not settle");
    }
}

pub fn pom(group_path: &str, artifact: &str, version: &str) -> String {
    format!(
        "<project><modelVersion>4.0.0</modelVersion><groupId>{}</groupId>\
         <artifactId>{artifact}</artifactId><version>{version}</version></project>",
        group_path.replace('/', ".")
    )
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

pub fn header_of<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

pub fn header_value(text: &str) -> HeaderValue {
    HeaderValue::from_str(text).unwrap()
}

pub fn status(response: &Response<Body>) -> u16 {
    response.status().as_u16()
}

/// The hex digest of `data`.
pub fn sha1_hex(data: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    hex::encode(Sha1::digest(data))
}

pub fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    hex::encode(Md5::digest(data))
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

pub fn sha512_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha512};
    hex::encode(Sha512::digest(data))
}

/// The `Content-Type` of a response as text.
pub fn content_type(response: &Response<Body>) -> String {
    header_of(response.headers(), header::CONTENT_TYPE.as_str())
        .unwrap_or_default()
        .to_owned()
}
