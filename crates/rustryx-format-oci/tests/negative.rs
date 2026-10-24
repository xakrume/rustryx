//! Negative requirements that need the engine: a wrong digest stores nothing, a mount does
//! not cross into a repository the caller cannot read, a chunk past the blob cap is refused
//! before it is staged, and an anonymous pull is refused unless the repository flag is on.
//! A proxy allow list is the other gate: a name it excludes is not fetched.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::large_futures,
    reason = "tests: a panic is the failure signal and a scenario reads best as straight-line code"
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use axum_core::body::Body;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::error::{AuthError, BlobError};
use rustryx_core::identity::{Action, AuthMethod, Principal};
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::{
    AssetRepo, BlobStore, Decision, RbacChecker, RepositoryRepo, ResourceRef,
};
use rustryx_format_core::testing::FakeUpstream;
use rustryx_format_core::{
    BlobStoreRegistry, FormatEngine, FormatRegistry, Limits, Ports, RequestHead,
};
use rustryx_format_oci::OciHandler;
use rustryx_format_oci::parse::{Digest, DigestAlgorithm};
use rustryx_testkit::fixtures;
use rustryx_testkit::{FixedClock, MemAuditSink, MemBlobStore, MemStore};
use serde_json::Value;
use time::OffsetDateTime;
use url::Url;

const MANIFEST_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// A minimal OCI image manifest. Its referenced blobs are never stored: a digest check
/// that runs first must refuse the document before those lookups.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","#,
    r#""config":{"mediaType":"application/vnd.oci.image.config.v1+json","#,
    r#""digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":2},"#,
    r#""layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","#,
    r#""digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","size":2}]}"#
);

struct FakeRbac {
    allow_all: bool,
    grants: Mutex<HashSet<(String, String, Action)>>,
}

impl FakeRbac {
    fn allow_all() -> Self {
        Self {
            allow_all: true,
            grants: Mutex::new(HashSet::new()),
        }
    }

    fn selective() -> Self {
        Self {
            allow_all: false,
            grants: Mutex::new(HashSet::new()),
        }
    }

    fn grant(&self, user: &str, repository: &str, actions: &[Action]) {
        let mut grants = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        for action in actions {
            grants.insert((user.to_owned(), repository.to_owned(), *action));
        }
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
        if self.allow_all {
            return Ok(Decision::Allow);
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
        if self.allow_all {
            return Ok(true);
        }
        Ok(self
            .grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(user, name, _)| user == &principal.username && name == repository))
    }
}

struct OneStore(Arc<dyn BlobStore>);

#[async_trait]
impl BlobStoreRegistry for OneStore {
    async fn store_by_id(&self, _id: BlobStoreId) -> Result<Arc<dyn BlobStore>, BlobError> {
        Ok(Arc::clone(&self.0))
    }
}

struct World {
    store: MemStore,
    blobs: MemBlobStore,
    engine: FormatEngine,
    upstream: Arc<FakeUpstream>,
    rbac: Arc<FakeRbac>,
}

impl World {
    fn open(rbac: FakeRbac, limits: Limits) -> Self {
        let mut registry = FormatRegistry::new();
        registry
            .register(Arc::new(OciHandler::default()))
            .expect("oci handler registers");
        let blobs = MemBlobStore::new();
        let clock = Arc::new(FixedClock::new(
            OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("timestamp"),
        ));
        let store = MemStore::with_builtin_roles()
            .with_clock(clock.clone())
            .with_default_blob_store();
        let rbac = Arc::new(rbac);
        let ports = Ports::from_store(
            Arc::new(store.clone()),
            rbac.clone(),
            Arc::new(MemAuditSink::new()),
            clock,
            Arc::new(OneStore(Arc::new(blobs.clone()))),
        );
        let upstream = FakeUpstream::new();
        let engine = FormatEngine::builder(ports, registry)
            .limits(limits)
            .upstream_client(upstream.clone())
            .build()
            .expect("engine builds");
        Self {
            store,
            blobs,
            engine,
            upstream,
            rbac,
        }
    }

    async fn hosted(&self, name: &str) -> Arc<Repository> {
        let repository = fixtures::repository(name, RepoFormat::Docker, RepoType::Hosted);
        RepositoryRepo::create(&self.store, &repository)
            .await
            .expect("repository creates");
        Arc::new(repository)
    }

    async fn proxy(&self, name: &str, allow: &[&str], anonymous: bool) -> Arc<Repository> {
        let mut repository = fixtures::repository(name, RepoFormat::Docker, RepoType::Proxy);
        repository.allow_anonymous = anonymous;
        let proxy = repository
            .proxy
            .as_mut()
            .expect("a proxy fixture has proxy settings");
        proxy.namespace_allowlist = allow.iter().map(|pattern| (*pattern).to_owned()).collect();
        RepositoryRepo::create(&self.store, &repository)
            .await
            .expect("repository creates");
        Arc::new(repository)
    }
}

fn user(name: &str) -> Principal {
    let mut principal = fixtures::principal_with_roles(&[]);
    name.clone_into(&mut principal.username);
    principal.auth_method = AuthMethod::BasicLocal;
    principal
}

fn base_url() -> Url {
    Url::parse("https://registry.example.test").expect("base url")
}

fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    headers
}

#[allow(
    clippy::too_many_arguments,
    reason = "one argument per part of the registry request; a struct would only rename them"
)]
async fn call(
    world: &World,
    repository: Arc<Repository>,
    method: Method,
    path: &str,
    query: Option<&str>,
    principal: Principal,
    headers: HeaderMap,
    body: &'static [u8],
) -> Response<Body> {
    let mut head = RequestHead::new(method, path, principal, base_url()).with_headers(headers);
    if let Some(query) = query {
        head = head.with_query(query);
    }
    world.engine.serve(repository, head, Body::from(body)).await
}

async fn error_of(response: Response<Body>) -> (StatusCode, String, String) {
    let status = response.status();
    let bytes = rustryx_format_core::stream::read_bounded(response.into_body(), 1 << 20)
        .await
        .expect("error body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let code = body["errors"][0]["code"].as_str().unwrap_or("").to_owned();
    (status, code, text)
}

async fn asset_missing(world: &World, repository: &Repository, path: &str) {
    let found = AssetRepo::get_by_path(&world.store, repository.id, path)
        .await
        .expect("asset lookup");
    assert!(found.is_none(), "stored {path}");
}

fn digest_of(bytes: &[u8]) -> String {
    Digest::of(DigestAlgorithm::Sha256, bytes).to_string()
}

fn not_the_digest(bytes: &[u8]) -> String {
    let mut text = digest_of(bytes);
    let flipped = if text.ends_with('0') { '1' } else { '0' };
    text.pop();
    text.push(flipped);
    text
}

#[tokio::test]
async fn must_not_store_a_blob_whose_digest_does_not_match() {
    let world = World::open(FakeRbac::allow_all(), Limits::default());
    let repository = world.hosted("docker").await;
    let body = b"layer-bytes";
    let declared = not_the_digest(body);
    let response = call(
        &world,
        Arc::clone(&repository),
        Method::POST,
        "v2/app/blobs/uploads",
        Some(&format!("digest={declared}")),
        user("publisher"),
        header_map(&[("content-length", "11")]),
        body,
    )
    .await;
    let (status, code, text) = error_of(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(code, "DIGEST_INVALID", "{text}");
    asset_missing(&world, &repository, &format!("app/blobs/{declared}")).await;
    assert_eq!(
        world.blobs.len(),
        0,
        "a mismatched blob must not stay in the store"
    );
}

#[tokio::test]
async fn must_not_store_a_manifest_whose_url_digest_does_not_match() {
    let world = World::open(FakeRbac::allow_all(), Limits::default());
    let repository = world.hosted("docker").await;
    let body = MANIFEST.as_bytes();
    let declared = not_the_digest(body);
    let response = call(
        &world,
        Arc::clone(&repository),
        Method::PUT,
        &format!("v2/app/manifests/{declared}"),
        None,
        user("publisher"),
        header_map(&[
            ("content-type", MANIFEST_TYPE),
            ("content-length", &body.len().to_string()),
        ]),
        body,
    )
    .await;
    let (status, code, text) = error_of(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(code, "DIGEST_INVALID", "{text}");
    assert!(
        !text.contains("MANIFEST_BLOB_UNKNOWN"),
        "the digest check must run before referenced blobs are required: {text}"
    );
    asset_missing(&world, &repository, &format!("app/manifests/{declared}")).await;
    let real = digest_of(body);
    asset_missing(&world, &repository, &format!("app/manifests/{real}")).await;
    assert_eq!(world.blobs.len(), 0);
}

#[tokio::test]
async fn must_not_mount_a_blob_from_a_repository_the_caller_cannot_read() {
    let world = World::open(FakeRbac::selective(), Limits::default());
    world
        .rbac
        .grant("owner", "source", &[Action::Read, Action::Write]);
    world
        .rbac
        .grant("owner", "target", &[Action::Read, Action::Write]);
    world.rbac.grant("writer", "target", &[Action::Write]);
    let source = world.hosted("source").await;
    let target = world.hosted("target").await;
    let body = b"shared-layer";
    let digest = digest_of(body);
    let pushed = call(
        &world,
        Arc::clone(&source),
        Method::POST,
        "v2/app/blobs/uploads",
        Some(&format!("digest={digest}")),
        user("owner"),
        header_map(&[("content-length", &body.len().to_string())]),
        body,
    )
    .await;
    assert_eq!(
        pushed.status(),
        StatusCode::CREATED,
        "the source blob has to exist"
    );
    let mounted = call(
        &world,
        Arc::clone(&target),
        Method::POST,
        "v2/app/blobs/uploads",
        Some(&format!("mount={digest}&from=source/app")),
        user("writer"),
        HeaderMap::new(),
        b"",
    )
    .await;
    assert_eq!(
        mounted.status(),
        StatusCode::ACCEPTED,
        "a refused mount starts an ordinary upload and must not say why"
    );
    asset_missing(&world, &target, &format!("app/blobs/{digest}")).await;
    let allowed = call(
        &world,
        Arc::clone(&target),
        Method::POST,
        "v2/app/blobs/uploads",
        Some(&format!("mount={digest}&from=source/app")),
        user("owner"),
        HeaderMap::new(),
        b"",
    )
    .await;
    assert_eq!(allowed.status(), StatusCode::CREATED);
    let linked = AssetRepo::get_by_path(&world.store, target.id, &format!("app/blobs/{digest}"))
        .await
        .expect("asset lookup");
    assert!(
        linked.is_some(),
        "a caller who can read the source may mount"
    );
}

#[tokio::test]
async fn must_not_accept_a_chunk_that_exceeds_the_blob_limit() {
    let limits = Limits {
        max_blob_bytes: 8,
        ..Limits::default()
    };
    let world = World::open(FakeRbac::allow_all(), limits);
    let repository = world.hosted("docker").await;
    let started = call(
        &world,
        Arc::clone(&repository),
        Method::POST,
        "v2/app/blobs/uploads",
        None,
        user("publisher"),
        HeaderMap::new(),
        b"",
    )
    .await;
    assert_eq!(started.status(), StatusCode::ACCEPTED);
    let location = started
        .headers()
        .get(header::LOCATION)
        .expect("upload location")
        .to_str()
        .expect("location is text")
        .to_owned();
    let session = location.rsplit('/').next().expect("session id");
    assert_eq!(world.blobs.len(), 0);
    let chunk = b"123456789";
    let response = call(
        &world,
        Arc::clone(&repository),
        Method::PATCH,
        &format!("v2/app/blobs/uploads/{session}"),
        None,
        user("publisher"),
        header_map(&[("content-length", "9")]),
        chunk,
    )
    .await;
    let (status, code, text) = error_of(response).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{text}");
    assert_eq!(code, "SIZE_INVALID", "{text}");
    assert_eq!(
        world.blobs.len(),
        0,
        "bytes over the cap must not become a blob"
    );
    let progress = call(
        &world,
        repository,
        Method::GET,
        &format!("v2/app/blobs/uploads/{session}"),
        None,
        user("publisher"),
        HeaderMap::new(),
        b"",
    )
    .await;
    assert_eq!(progress.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        progress
            .headers()
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("0-0"),
        "the chunk must be refused before any byte is staged"
    );
}

#[tokio::test]
async fn must_not_serve_an_anonymous_pull_without_the_repository_flag() {
    let world = World::open(FakeRbac::allow_all(), Limits::default());
    let repository = world.hosted("docker").await;
    assert!(
        !repository.allow_anonymous,
        "the fixture must stay closed; this test is the anonymous gate"
    );
    let body = b"secret-layer";
    let digest = digest_of(body);
    let pushed = call(
        &world,
        Arc::clone(&repository),
        Method::POST,
        "v2/app/blobs/uploads",
        Some(&format!("digest={digest}")),
        user("publisher"),
        header_map(&[("content-length", &body.len().to_string())]),
        body,
    )
    .await;
    assert_eq!(pushed.status(), StatusCode::CREATED);
    let denied = call(
        &world,
        Arc::clone(&repository),
        Method::GET,
        &format!("v2/app/blobs/{digest}"),
        None,
        Principal::anonymous(),
        HeaderMap::new(),
        b"",
    )
    .await;
    let (status, code, text) = error_of(denied).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
    assert_eq!(code, "UNAUTHORIZED", "{text}");
    assert!(
        !text.contains("secret-layer"),
        "a refused pull must not carry the blob"
    );
    let mut open = (*repository).clone();
    open.allow_anonymous = true;
    let allowed = call(
        &world,
        Arc::new(open),
        Method::GET,
        &format!("v2/app/blobs/{digest}"),
        None,
        Principal::anonymous(),
        HeaderMap::new(),
        b"",
    )
    .await;
    assert_eq!(allowed.status(), StatusCode::OK);
    let bytes = rustryx_format_core::stream::read_bounded(allowed.into_body(), 1 << 20)
        .await
        .expect("blob body");
    assert_eq!(bytes.as_ref(), body);
}

#[tokio::test]
async fn must_not_pull_through_a_proxy_outside_the_namespace_allow_list() {
    let world = World::open(FakeRbac::allow_all(), Limits::default());
    let repository = world.proxy("docker", &["allowed/*"], true).await;
    let denied = call(
        &world,
        Arc::clone(&repository),
        Method::GET,
        "v2/secret/app/manifests/latest",
        None,
        Principal::anonymous(),
        HeaderMap::new(),
        b"",
    )
    .await;
    let (status, code, text) = error_of(denied).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert_eq!(code, "NAME_UNKNOWN", "{text}");
    assert_eq!(
        world.upstream.calls(),
        0,
        "an excluded name must not reach the upstream"
    );
    let broken = world.proxy("broken", &["}"], true).await;
    let still_closed = call(
        &world,
        broken,
        Method::GET,
        "v2/allowed/app/manifests/latest",
        None,
        Principal::anonymous(),
        HeaderMap::new(),
        b"",
    )
    .await;
    let (status, code, text) = error_of(still_closed).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert_eq!(code, "NAME_UNKNOWN", "{text}");
    assert_eq!(
        world.upstream.calls(),
        0,
        "a pattern that does not compile must not open the proxy"
    );
}
