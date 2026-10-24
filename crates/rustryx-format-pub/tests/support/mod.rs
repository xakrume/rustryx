//! The in-memory world of the contract tests: metadata store, blob store, scripted upstream, a
//! scripted authorizer and the engine, with `PubHandler` registered.
#![allow(
    dead_code,
    reason = "each integration test binary uses a different subset of the harness"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::large_futures,
    clippy::needless_pass_by_value,
    clippy::duration_suboptimal_units,
    clippy::unused_self,
    clippy::similar_names,
    reason = "test scenarios read best as straight-line code with local helpers"
)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::error::{AuthError, BlobError};
use rustryx_core::identity::{Action, AuthMethod, Principal};
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{GroupConfig, RepoFormat, RepoType, Repository};
use rustryx_core::ports::{BlobStore, Decision, RbacChecker, RepositoryRepo, ResourceRef};
use rustryx_format_core::proxy::breaker::BreakerConfig;
use rustryx_format_core::proxy::executor::{ExecutorConfig, RetryPolicy};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use rustryx_format_core::testing::FakeUpstream;
use rustryx_format_core::{
    BlobStoreRegistry, ContentEvent, ContentEvents, FormatEngine, FormatRegistry, Limits, Ports,
    RequestHead,
};
use rustryx_format_pub::testing::{multipart, multipart_content_type, package_archive};
use rustryx_format_pub::{PubConfig, PubHandler};
use rustryx_testkit::fixtures;
use rustryx_testkit::{FixedClock, MemAuditSink, MemBlobStore, MemStore};
use serde_json::Value;
use time::OffsetDateTime;
use url::Url;

/// The address the tests pretend to be served from.
pub const BASE: &str = "https://rustryx.test";

/// Media type of every pub answer.
pub const PUB_JSON: &str = "application/vnd.pub.v2+json";

/// A scripted authorizer: explicit grants per user, repository and action, a rule that denies one
/// package whatever the grant says, a switch that makes it fail, and a log of every check.
#[derive(Default)]
pub struct FakeRbac {
    grants: Mutex<HashSet<(String, String, Action)>>,
    denied_packages: Mutex<HashSet<String>>,
    failing_repositories: Mutex<HashSet<String>>,
    fail: AtomicBool,
    checks: Mutex<Vec<Check>>,
}

/// One authorization check as the authorizer saw it.
#[derive(Debug, Clone)]
pub struct Check {
    pub user: String,
    pub repository: String,
    pub action: Action,
    pub path: String,
    pub name: Option<String>,
    pub version: Option<String>,
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

    /// Writes of this package name are denied to everybody, like a content selector would.
    pub fn deny_package(&self, name: &str) {
        self.denied_packages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned());
    }

    pub fn fail_repository(&self, repository: &str) {
        self.failing_repositories
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(repository.to_owned());
    }

    pub fn set_failing(&self, failing: bool) {
        self.fail.store(failing, Ordering::SeqCst);
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
        let (name, version) = resource
            .coordinate
            .as_ref()
            .map_or((None, None), |c| (c.name.clone(), c.version.clone()));
        self.checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Check {
                user: principal.username.clone(),
                repository: resource.repository.clone(),
                action,
                path: resource.path.clone(),
                name: name.clone(),
                version,
            });
        if self.fail.load(Ordering::SeqCst)
            || self
                .failing_repositories
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&resource.repository)
        {
            return Err(AuthError::GroupLookupFailed("provider down".into()));
        }
        if action == Action::Write
            && name.is_some_and(|name| {
                self.denied_packages
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .contains(&name)
            })
        {
            return Ok(Decision::Deny);
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

    /// Paths of the assets that were published, in order.
    pub fn published_paths(&self) -> Vec<String> {
        self.events()
            .iter()
            .filter_map(|event| match event {
                ContentEvent::AssetPublished { asset, .. } => Some(asset.path.clone()),
                _ => None,
            })
            .collect()
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

/// An authenticated principal named `name`. Create it once per test and clone it: the id of the
/// user is generated here, and the uploader of a publication is compared by id.
pub fn principal(name: &str) -> Principal {
    let mut principal = fixtures::principal_with_roles(&[]);
    name.clone_into(&mut principal.username);
    principal.auth_method = AuthMethod::ApiToken;
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
    pub events: Arc<RecordingEvents>,
    pub engine: FormatEngine,
    pub ports: Ports,
}

/// What a finished response looked like.
pub struct Answer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Answer {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not json ({e}): {:?}", String::from_utf8_lossy(&self.body)))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn error_code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    pub fn error_message(&self) -> String {
        self.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// The answer is a pub error: the media type, the object, and for 401/403 the challenge.
    pub fn assert_pub_error(&self, status: StatusCode) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        assert_eq!(self.header("content-type"), Some(PUB_JSON));
        let body = self.json();
        assert!(body["error"]["code"].is_string(), "{body}");
        assert!(body["error"]["message"].is_string(), "{body}");
        assert!(body.get("request_id").is_none() && body["error"].get("request_id").is_none());
        let challenge = self.header("www-authenticate");
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            let challenge = challenge.expect("401 and 403 carry a challenge");
            assert!(
                challenge.starts_with("Bearer realm=\"pub\", message=\""),
                "{challenge}"
            );
        } else {
            assert_eq!(challenge, None);
        }
    }
}

impl World {
    pub fn new() -> Self {
        Self::build(PubConfig::default(), Limits::default(), |_| {})
    }

    pub fn with_config(config: PubConfig) -> Self {
        Self::build(config, Limits::default(), |_| {})
    }

    pub fn with_ports(customize: impl FnOnce(&mut Ports)) -> Self {
        Self::build(PubConfig::default(), Limits::default(), customize)
    }

    pub fn build(config: PubConfig, limits: Limits, customize: impl FnOnce(&mut Ports)) -> Self {
        let mut registry = FormatRegistry::new();
        registry
            .register(Arc::new(PubHandler::new(config)))
            .unwrap();
        let blobs = MemBlobStore::new();
        let stores: Arc<dyn BlobStoreRegistry> = Arc::new(SingleStore(Arc::new(blobs.clone())));
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
        let engine = FormatEngine::builder(ports.clone(), registry)
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
            events,
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

    pub async fn hosted(&self, name: &str) -> Arc<Repository> {
        self.add(fixtures::repository(
            name,
            RepoFormat::Pub,
            RepoType::Hosted,
        ))
        .await
    }

    pub async fn hosted_with(
        &self,
        name: &str,
        change: impl FnOnce(&mut Repository),
    ) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Pub, RepoType::Hosted);
        change(&mut repository);
        self.add(repository).await
    }

    pub async fn proxy_at(&self, name: &str, remote: &str) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Pub, RepoType::Proxy);
        remote.clone_into(&mut repository.proxy.as_mut().unwrap().remote_url);
        self.add(repository).await
    }

    pub async fn group(&self, name: &str, members: &[&Arc<Repository>]) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Pub, RepoType::Group);
        repository.group = Some(GroupConfig {
            members: members.iter().map(|m| m.name.clone()).collect(),
        });
        self.add(repository).await
    }

    pub fn head(&self, method: Method, path: &str, who: &Principal) -> RequestHead {
        RequestHead::new(method, path, who.clone(), Url::parse(BASE).unwrap())
    }

    pub async fn serve(
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
    ) -> Answer {
        let head = self.head(method, path, who);
        answer(self.serve(repository, head, body).await).await
    }

    pub async fn get(&self, repository: &Arc<Repository>, path: &str, who: &Principal) -> Answer {
        self.send(repository, Method::GET, path, who, Body::empty())
            .await
    }

    /// `GET` with an `Accept` header.
    pub async fn get_accepting(
        &self,
        repository: &Arc<Repository>,
        path: &str,
        who: &Principal,
        accept: &str,
    ) -> Answer {
        let mut head = self.head(Method::GET, path, who);
        head.headers
            .insert(header::ACCEPT, HeaderValue::from_str(accept).unwrap());
        answer(self.serve(repository, head, Body::empty()).await).await
    }

    /// `GET` of a version listing, with the media type a real client sends.
    pub async fn list(
        &self,
        repository: &Arc<Repository>,
        package: &str,
        who: &Principal,
    ) -> Answer {
        self.get_accepting(
            repository,
            &format!("api/packages/{package}"),
            who,
            PUB_JSON,
        )
        .await
    }

    /// Step 1 of a publication. Returns the answer and, on success, the path of the upload URL.
    pub async fn begin(
        &self,
        repository: &Arc<Repository>,
        who: &Principal,
        query: Option<&str>,
    ) -> (Answer, Option<String>) {
        let mut head = self.head(Method::GET, "api/packages/versions/new", who);
        head.headers
            .insert(header::ACCEPT, HeaderValue::from_static(PUB_JSON));
        if let Some(query) = query {
            head = head.with_query(query);
        }
        let answer = answer(self.serve(repository, head, Body::empty()).await).await;
        let path = (answer.status == StatusCode::OK).then(|| {
            let body = answer.json();
            assert_eq!(body["fields"], serde_json::json!({}));
            path_below(repository, body["url"].as_str().unwrap())
        });
        (answer, path)
    }

    /// Step 2: the multipart upload of `file` to `upload_path`.
    pub async fn upload(
        &self,
        repository: &Arc<Repository>,
        who: &Principal,
        upload_path: &str,
        file: &[u8],
    ) -> Answer {
        let body = multipart(&[], file);
        self.upload_body(repository, who, upload_path, body.len(), Body::from(body))
            .await
    }

    pub async fn upload_body(
        &self,
        repository: &Arc<Repository>,
        who: &Principal,
        upload_path: &str,
        declared_length: usize,
        body: Body,
    ) -> Answer {
        let mut head = self.head(Method::POST, upload_path, who);
        head.headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&multipart_content_type()).unwrap(),
        );
        head.headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from(declared_length as u64),
        );
        answer(self.serve(repository, head, body).await).await
    }

    /// Step 3: finalize, at the `Location` of the upload answer.
    pub async fn finish(
        &self,
        repository: &Arc<Repository>,
        who: &Principal,
        uploaded: &Answer,
    ) -> Answer {
        assert_eq!(
            uploaded.status,
            StatusCode::NO_CONTENT,
            "{}",
            String::from_utf8_lossy(&uploaded.body)
        );
        let location = uploaded.header("location").expect("Location").to_owned();
        let path = path_below(repository, &location);
        let mut head = self.head(Method::GET, &path, who);
        head.headers
            .insert(header::ACCEPT, HeaderValue::from_static(PUB_JSON));
        answer(self.serve(repository, head, Body::empty()).await).await
    }

    /// The whole publication of `archive` by `who`. Returns the answer of the last step that ran.
    pub async fn publish(
        &self,
        repository: &Arc<Repository>,
        who: &Principal,
        archive: &[u8],
    ) -> Answer {
        let (begun, path) = self.begin(repository, who, None).await;
        let Some(path) = path else { return begun };
        let uploaded = self.upload(repository, who, &path, archive).await;
        if uploaded.status != StatusCode::NO_CONTENT {
            return uploaded;
        }
        self.finish(repository, who, &uploaded).await
    }

    /// Publishes a generated package and asserts success. Returns the archive bytes.
    pub async fn publish_ok(
        &self,
        repository: &Arc<Repository>,
        who: &Principal,
        name: &str,
        version: &str,
    ) -> Vec<u8> {
        let archive = package_archive(name, version);
        let done = self.publish(repository, who, &archive).await;
        assert_eq!(
            done.status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&done.body)
        );
        assert_eq!(
            done.json()["success"]["message"],
            format!("Successfully uploaded {name} version {version}.")
        );
        archive
    }

    /// Paths of every asset of the repository (all pages).
    pub async fn asset_paths(&self, repository: &Repository, prefix: &str) -> Vec<String> {
        use rustryx_core::ports::{AssetRepo, PageRequest};
        let assets: &dyn AssetRepo = &self.store;
        let mut paths = Vec::new();
        let mut after = None;
        loop {
            let page = assets
                .list(
                    repository.id,
                    prefix,
                    &PageRequest {
                        limit: 500,
                        after: after.take(),
                    },
                )
                .await
                .unwrap();
            paths.extend(page.items.into_iter().map(|a| a.path));
            match page.next {
                Some(token) => after = Some(token),
                None => break,
            }
        }
        paths
    }

    pub async fn staged(&self, repository: &Repository) -> Vec<String> {
        self.asset_paths(repository, "api/packages/versions/upload/")
            .await
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

/// The path of a URL below the repository, as a client would request it.
pub fn path_below(repository: &Repository, url: &str) -> String {
    let prefix = format!("{BASE}/repository/{}/", repository.name);
    url.strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("{url} is not under {prefix}"))
        .to_owned()
}

pub async fn answer(response: Response<Body>) -> Answer {
    let (parts, body) = response.into_parts();
    let body = rustryx_format_core::stream::read_bounded(body, u64::MAX)
        .await
        .unwrap_or_default();
    Answer {
        status: parts.status,
        headers: parts.headers,
        body,
    }
}

/// A request body fed from the test: send chunks, drop the sender to end the body.
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

/// A body that counts the bytes that were pulled from it and never ends.
pub fn endless_body(prefix: Vec<u8>, pulled: Arc<std::sync::atomic::AtomicU64>) -> Body {
    let stream = futures::stream::unfold(Some(prefix), move |state| {
        let pulled = Arc::clone(&pulled);
        async move {
            let chunk = match state {
                Some(prefix) => Bytes::from(prefix),
                None => Bytes::from(vec![0x5a; 1024]),
            };
            pulled.fetch_add(chunk.len() as u64, Ordering::SeqCst);
            Some((Ok::<_, std::io::Error>(chunk), None))
        }
    });
    Body::from_stream(stream)
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

/// One version an upstream offers.
pub struct Offered<'a> {
    pub version: &'a str,
    pub archive: &'a [u8],
    pub retracted: bool,
}

impl<'a> Offered<'a> {
    pub fn new(version: &'a str, archive: &'a [u8]) -> Self {
        Self {
            version,
            archive,
            retracted: false,
        }
    }

    pub fn retracted(mut self) -> Self {
        self.retracted = true;
        self
    }
}

/// A listing the way the public registry writes it: `latest`, `versions`, the upstream's own
/// `archive_url`, the digests, and an `advisoriesUpdated` that must never reach a client.
pub fn upstream_listing(package: &str, offered: &[Offered<'_>]) -> Vec<u8> {
    let entry = |o: &Offered<'_>| {
        let mut entry = serde_json::json!({
            "version": o.version,
            "archive_url": format!("https://pub.example/api/archives/{package}-{}.tar.gz", o.version),
            "archive_sha256": rustryx_format_pub::testing::sha256_hex(o.archive),
            "pubspec": { "name": package, "version": o.version, "environment": { "sdk": "^3.0.0" } },
            "published": "2024-05-06T07:08:09.000000Z",
        });
        if o.retracted {
            entry["retracted"] = serde_json::json!(true);
        }
        entry
    };
    let entries: Vec<Value> = offered.iter().map(entry).collect();
    serde_json::to_vec(&serde_json::json!({
        "name": package,
        "advisoriesUpdated": "2024-05-06T07:08:09.000000Z",
        "latest": entries.last().cloned().unwrap_or(Value::Null),
        "versions": entries,
    }))
    .unwrap()
}

/// Scripts an upstream for one package: the listing and each archive.
pub fn script_upstream(world: &World, package: &str, offered: &[Offered<'_>]) {
    script_upstream_at(world, "", package, offered);
}

/// Like [`script_upstream`] for a proxy whose `remote_url` has the path `prefix` (`/a`), so that
/// two proxies can have different upstreams on the one scripted fake.
pub fn script_upstream_at(world: &World, prefix: &str, package: &str, offered: &[Offered<'_>]) {
    use rustryx_format_core::testing::Reply;
    world.upstream.route(
        &format!("{prefix}/api/packages/{package}"),
        vec![Reply::ok(upstream_listing(package, offered)).header("content-type", PUB_JSON)],
    );
    for o in offered {
        world.upstream.route(
            &format!("{prefix}/packages/{package}/versions/{}.tar.gz", o.version),
            vec![Reply::ok(o.archive.to_vec()).header("content-type", "application/octet-stream")],
        );
    }
}
