use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use http::HeaderValue;
use rustryx_api::{AppState, RouterBuilder};
use rustryx_blob::{BlobStoreRegistry, MapSecretResolver};
use rustryx_core::Secret;
use rustryx_core::error::{AuthError, StoreError};
use rustryx_core::identity::{AuthContext, Credentials, Principal};
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::{Authenticator, RepositoryRepo, SystemClock};
use rustryx_format_core::context::Ports;
use rustryx_format_core::{FormatEngine, FormatRegistry};
use rustryx_format_oci::{CatalogService, OciConfig, OciTokenService};
use url::Url;

use crate::wiring::formats;
use crate::wiring::registry_login::RegistryLogin;
use rustryx_rbac::{Rbac, RbacConfig};
use rustryx_testkit::fixtures::repository;
use rustryx_testkit::{MemAuditSink, MemStore};
use tower::ServiceExt;

use super::*;
use crate::wiring::blobs::FormatStores;

/// Claims nothing. A request with no credential stays anonymous. A bearer is left for the
/// registry hook, and is a 401 when that hook is not installed or rejects it.
struct Anonymous;

#[async_trait]
impl Authenticator for Anonymous {
    async fn authenticate(
        &self,
        _credentials: &Credentials,
        _context: &AuthContext,
    ) -> Result<Option<Principal>, AuthError> {
        Ok(None)
    }
}

/// Counts lookups, to show that an invalid name never reaches the database.
struct Counting {
    inner: MemStore,
    lookups: AtomicUsize,
    fail: bool,
}

#[async_trait]
impl RepositoryRepo for Counting {
    async fn create(&self, repository: &Repository) -> Result<(), StoreError> {
        RepositoryRepo::create(&self.inner, repository).await
    }

    async fn get(
        &self,
        id: rustryx_core::ids::RepositoryId,
    ) -> Result<Option<Repository>, StoreError> {
        RepositoryRepo::get(&self.inner, id).await
    }

    async fn get_by_name(&self, name: &RepoName) -> Result<Option<Repository>, StoreError> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(StoreError::Unavailable("database is down".into()));
        }
        RepositoryRepo::get_by_name(&self.inner, name).await
    }

    async fn list(&self) -> Result<Vec<Repository>, StoreError> {
        RepositoryRepo::list(&self.inner).await
    }

    async fn update(&self, repository: &Repository) -> Result<(), StoreError> {
        RepositoryRepo::update(&self.inner, repository).await
    }

    async fn delete(&self, id: rustryx_core::ids::RepositoryId) -> Result<(), StoreError> {
        RepositoryRepo::delete(&self.inner, id).await
    }
}

fn distribution(store: &MemStore) -> (OciTokenService, CatalogService) {
    let rbac = Arc::new(Rbac::new(
        Arc::new(store.clone()),
        Arc::new(store.clone()),
        &RbacConfig::default(),
    ));
    let tokens = formats::token_service(
        Arc::new(store.clone()),
        Arc::clone(&rbac) as Arc<dyn rustryx_core::ports::RbacChecker>,
        Arc::new(SystemClock),
        &Secret::new("test-cookie-key"),
    )
    .expect("token service");
    let catalog = CatalogService::new(
        Arc::new(store.clone()),
        Arc::new(store.clone()),
        rbac,
        OciConfig::default(),
    );
    (tokens, catalog)
}

fn module(
    engine: FormatEngine,
    repos: Arc<dyn RepositoryRepo>,
    store: &MemStore,
    base_url: Option<Url>,
    require_https: bool,
) -> (ArtifactModule, Arc<RegistryLogin>) {
    let (tokens, catalog) = distribution(store);
    let login = Arc::new(RegistryLogin::new(tokens.clone(), Arc::new(store.clone())));
    let module = ArtifactModule::new(
        engine,
        repos,
        base_url,
        require_https,
        tokens,
        catalog,
        None,
    );
    (module, login)
}

fn engine(store: &MemStore) -> FormatEngine {
    let rbac = Arc::new(Rbac::new(
        Arc::new(store.clone()),
        Arc::new(store.clone()),
        &RbacConfig::default(),
    ));
    let registry = Arc::new(BlobStoreRegistry::new(
        Arc::new(store.clone()),
        Arc::new(MapSecretResolver::default()),
    ));
    let ports = Ports::from_store(
        Arc::new(store.clone()),
        rbac,
        Arc::new(MemAuditSink::new()),
        Arc::new(SystemClock),
        Arc::new(FormatStores::new(registry)),
    );
    FormatEngine::builder(ports, FormatRegistry::new())
        .build()
        .expect("engine")
}

struct Harness {
    router: axum::Router,
    repos: Arc<Counting>,
}

async fn harness(base_url: Option<&str>, fail_lookups: bool) -> Harness {
    let store = MemStore::with_builtin_roles().with_default_blob_store();
    let hosted = repository("releases", RepoFormat::Raw, RepoType::Hosted);
    RepositoryRepo::create(&store, &hosted)
        .await
        .expect("repository");
    let repos = Arc::new(Counting {
        inner: store.clone(),
        lookups: AtomicUsize::new(0),
        fail: fail_lookups,
    });
    let (module, login) = module(
        engine(&store),
        Arc::clone(&repos) as Arc<dyn RepositoryRepo>,
        &store,
        base_url.map(|url| Url::parse(url).expect("url")),
        false,
    );
    let state = AppState::unwired()
        .with_authenticator(Arc::new(Anonymous))
        .with_registry_bearer(login);
    let router = RouterBuilder::new(state).module(Arc::new(module)).build();
    Harness { router, repos }
}

async fn call(router: &axum::Router, method: &str, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn call_auth(
    router: &axum::Router,
    method: &str,
    uri: &str,
    authorization: &str,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization)
        .body(axum::body::Body::empty())
        .expect("request");
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[test]
fn the_path_below_the_repository_stays_raw_and_loses_its_leading_slash() {
    assert_eq!(split_path("/repository/releases"), Some(("releases", "")));
    assert_eq!(split_path("/repository/releases/"), Some(("releases", "")));
    assert_eq!(
        split_path("/repository/releases/a%2Fb/c.txt"),
        Some(("releases", "a%2Fb/c.txt")),
        "the format decodes the path exactly once, not this layer"
    );
    assert_eq!(split_path("/repository//x"), Some(("", "x")));
    assert_eq!(split_path("/other/x"), None);
}

#[test]
fn engine_labels_become_metric_labels_one_to_one() {
    let cases = [
        (FormatClass::Metadata, RequestClass::Metadata),
        (FormatClass::Download, RequestClass::Download),
        (FormatClass::Upload, RequestClass::Upload),
        (FormatClass::Api, RequestClass::Api),
        (FormatClass::Other, RequestClass::Other),
    ];
    for (from, to) in cases {
        let labels = metric_labels(FormatLabels::new(from, CacheLabel::None));
        assert_eq!(labels.class, Some(to));
    }
    let caches = [
        (CacheLabel::Hit, CacheStatus::Hit),
        (CacheLabel::Miss, CacheStatus::Miss),
        (CacheLabel::Stale, CacheStatus::Stale),
        (CacheLabel::Bypass, CacheStatus::Bypass),
        (CacheLabel::None, CacheStatus::Uncached),
    ];
    for (from, to) in caches {
        let labels = metric_labels(FormatLabels::new(FormatClass::Download, from));
        assert_eq!(labels.cache, Some(to));
        assert_eq!(
            labels.cache.map(CacheStatus::as_str),
            Some(from.as_str()),
            "the two vocabularies print the same words"
        );
    }
}

#[tokio::test]
async fn an_unknown_repository_is_a_404_with_the_error_envelope() {
    let h = harness(None, false).await;
    let (status, body) = call(&h.router, "GET", "/repository/nope/a.txt").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("repository_not_found"), "{body}");
}

#[tokio::test]
async fn must_not_look_up_a_name_that_cannot_be_a_repository() {
    let h = harness(None, false).await;
    for uri in [
        "/repository/bad..name/x",
        "/repository/-lead/x",
        "/repository/%2e%2e/x",
        "/repository//x",
    ] {
        let (status, _) = call(&h.router, "GET", uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    assert_eq!(
        h.repos.lookups.load(Ordering::SeqCst),
        0,
        "an invalid name is answered without a database read"
    );
}

#[tokio::test]
async fn a_database_failure_is_a_503_not_a_404() {
    let h = harness(None, true).await;
    let (status, body) = call(&h.router, "GET", "/repository/releases/a.txt").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        !body.contains("database is down"),
        "driver text stays in the logs: {body}"
    );
}

#[tokio::test]
async fn an_existing_repository_without_a_registered_format_is_the_engines_own_500() {
    let h = harness(Some("https://repo.example"), false).await;
    for method in ["GET", "HEAD", "PUT", "DELETE"] {
        let (status, _) = call(&h.router, method, "/repository/releases/a/b.txt?x=1").await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{method}: the repository exists and nothing can serve its format"
        );
    }
    assert_eq!(h.repos.lookups.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn the_repository_root_and_its_trailing_slash_reach_the_same_handler() {
    let h = harness(None, false).await;
    for uri in ["/repository/releases", "/repository/releases/"] {
        let (status, _) = call(&h.router, "GET", uri).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
    }
}

#[tokio::test]
async fn the_distribution_ping_challenges_and_a_registry_token_satisfies_it() {
    let h = harness(Some("https://repo.example"), false).await;
    for uri in ["/v2", "/v2/"] {
        let (status, body) = call(&h.router, "GET", uri).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} {body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(json["errors"][0]["code"], "UNAUTHORIZED");
    }
    let (status, body) = call(&h.router, "GET", "/v2/token?service=rustryx").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = serde_json::from_str::<serde_json::Value>(&body).expect("json")["token"]
        .as_str()
        .expect("token")
        .to_owned();
    let (status, body) = call_auth(&h.router, "GET", "/v2/", &format!("Bearer {token}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "{}");

    let (status, body) = call(&h.router, "GET", "/v2/library/app/manifests/latest").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["errors"][0]["code"], "NAME_UNKNOWN");

    let (status, _) = call_auth(&h.router, "GET", "/v2/", "Bearer not-a-token").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_artifact_routes_are_declared_as_delegated_to_the_format_engine() {
    let h = harness(None, false).await;
    drop(h);
    let state = AppState::unwired().with_authenticator(Arc::new(Anonymous));
    let store = MemStore::with_builtin_roles();
    let (module, _) = module(engine(&store), Arc::new(store.clone()), &store, None, true);
    let (_router, catalog) = RouterBuilder::new(state)
        .module(Arc::new(module))
        .build_with_catalog();
    assert!(
        catalog
            .all()
            .any(|decl| matches!(decl.requires, rustryx_api::Requires::Delegated(reason) if reason.contains("format engine"))),
        "deny by default rests on the engine, and the catalogue says so"
    );
}

#[test]
fn the_base_url_is_the_configured_one_or_the_host_header() {
    let state = |base: Option<&str>, https: bool| {
        let store = MemStore::new();
        module(
            engine(&store),
            Arc::new(store.clone()),
            &store,
            base.map(|url| Url::parse(url).unwrap()),
            https,
        )
        .0
        .state
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::HOST,
        HeaderValue::from_static("node.internal:8081"),
    );

    let configured = state(Some("https://repo.example/"), false);
    assert_eq!(
        base_url(&configured, &headers).unwrap().as_str(),
        "https://repo.example/"
    );
    let dev = state(None, false);
    assert_eq!(
        base_url(&dev, &headers).unwrap().as_str(),
        "http://node.internal:8081/"
    );
    let prod = state(None, true);
    assert_eq!(
        base_url(&prod, &headers).unwrap().scheme(),
        "https",
        "without a configured URL the safe scheme is https"
    );
    let mut bad = HeaderMap::new();
    bad.insert(http::header::HOST, HeaderValue::from_static("exa mple"));
    let error = base_url(&dev, &bad).expect_err("not an authority");
    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(
        !format!("{error:?}").contains("exa mple"),
        "a bad Host is never echoed"
    );
    assert!(
        base_url(&dev, &HeaderMap::new()).is_ok(),
        "a request without Host falls back to localhost"
    );
}

#[test]
fn must_not_log_the_authorization_header_value() {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Basic canary-credentials"),
    );
    let marked = sensitive_authorization(headers);
    let value = marked.get(AUTHORIZATION).expect("kept");
    assert!(value.is_sensitive());
    assert!(!format!("{marked:?}").contains("canary-credentials"));
}
