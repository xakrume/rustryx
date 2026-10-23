//! Registry, engine ordering and the per-format hooks.
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

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use http::{HeaderValue, Method, Response, StatusCode, header};
use rustryx_core::error::AuthError;
use rustryx_core::identity::{Action, Principal};
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::{Decision, RbacChecker, ResourceRef};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use rustryx_format_core::{
    ArtifactRequest, ErrorInfo, FormatContext, FormatEngine, FormatError, FormatHandler,
    FormatRegistry, Ports, RegistryError, RequestHead, classify_path_addressed,
};
use rustryx_testkit::fixtures;
use support::{
    FakeRbac, RawLikeHandler, SingleStore, World, anonymous, body_text, header_of, principal,
};

/// A handler that claims two labels, like the OCI handler.
struct TwoLabels;

#[async_trait]
impl FormatHandler for TwoLabels {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Docker, RepoFormat::Oci]
    }
    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        classify_path_addressed(repository, head)
    }
    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        cx.serve_default(req, body).await
    }
}

/// A handler that declares nothing.
struct NoLabels;

#[async_trait]
impl FormatHandler for NoLabels {
    fn formats(&self) -> &'static [RepoFormat] {
        &[]
    }
    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        classify_path_addressed(repository, head)
    }
    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        cx.serve_default(req, body).await
    }
}

/// pub-like: 403 (not 404) for hidden repositories, protocol shaped errors.
struct PubLike;

#[async_trait]
impl FormatHandler for PubLike {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Pub]
    }
    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        classify_path_addressed(repository, head)
    }
    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        cx.serve_default(req, body).await
    }
    fn hide_inaccessible_repository(&self) -> bool {
        false
    }
    fn render_error(&self, error: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
        let body = serde_json::json!({ "error": { "code": error.code(), "message": error.public_message() } });
        let mut response = Response::new(Body::from(body.to_string()));
        *response.status_mut() = error.status();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.pub.v2+json"),
        );
        if matches!(error, FormatError::Unauthenticated | FormatError::Forbidden) {
            let challenge = if info.anonymous {
                "Bearer realm=\"pub\""
            } else {
                "Bearer realm=\"pub\", message=\"no access\""
            };
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_str(challenge).unwrap(),
            );
        }
        response
    }
}

fn engine_with(handler: Arc<dyn FormatHandler>, world: &World) -> FormatEngine {
    let mut registry = FormatRegistry::new();
    registry.register(handler).unwrap();
    let ports = Ports::from_store(
        Arc::new(world.store.clone()),
        world.rbac.clone(),
        Arc::new(world.audit.clone()),
        world.clock.clone(),
        Arc::new(SingleStore(Arc::new(world.blobs.clone()))),
    );
    FormatEngine::builder(ports, registry)
        .url_policy(UpstreamUrlPolicy::any_public())
        .upstream_client(world.upstream.clone())
        .build()
        .unwrap()
}

#[test]
fn registry_resolves_one_handler_for_both_oci_labels() {
    let handler: Arc<dyn FormatHandler> = Arc::new(TwoLabels);
    let mut registry = FormatRegistry::new();
    registry.register(handler).unwrap();
    let docker = registry.get(RepoFormat::Docker).unwrap();
    let oci = registry.get(RepoFormat::Oci).unwrap();
    assert!(Arc::ptr_eq(docker, oci), "docker and oci share one handler");
    assert!(registry.get(RepoFormat::Maven2).is_none());
    assert_eq!(
        registry.formats(),
        vec![RepoFormat::Docker, RepoFormat::Oci]
    );
}

#[test]
fn registry_refuses_duplicates_and_empty_handlers_and_stays_unchanged() {
    let mut registry = FormatRegistry::new();
    registry.register(Arc::new(RawLikeHandler)).unwrap();
    assert_eq!(
        registry.register(Arc::new(RawLikeHandler)).unwrap_err(),
        RegistryError::Duplicate(RepoFormat::Raw)
    );
    assert_eq!(
        registry.register(Arc::new(NoLabels)).unwrap_err(),
        RegistryError::NoFormats
    );

    // A handler of two labels of which one is taken must not be half registered.
    let mut registry = FormatRegistry::new();
    registry.register(Arc::new(TwoLabels)).unwrap();
    struct DockerAndRaw;
    #[async_trait]
    impl FormatHandler for DockerAndRaw {
        fn formats(&self) -> &'static [RepoFormat] {
            &[RepoFormat::Raw, RepoFormat::Docker]
        }
        fn classify(
            &self,
            r: &Repository,
            h: &RequestHead,
        ) -> Result<ArtifactRequest, FormatError> {
            classify_path_addressed(r, h)
        }
        async fn handle(
            &self,
            cx: FormatContext,
            req: ArtifactRequest,
            body: Body,
        ) -> Result<Response<Body>, FormatError> {
            cx.serve_default(req, body).await
        }
    }
    assert!(registry.register(Arc::new(DockerAndRaw)).is_err());
    assert!(
        registry.get(RepoFormat::Raw).is_none(),
        "registration is all or nothing"
    );
    let chained = FormatRegistry::new()
        .with(Arc::new(RawLikeHandler))
        .unwrap();
    assert!(chained.get(RepoFormat::Raw).is_some());
}

#[tokio::test]
async fn a_repository_without_a_registered_format_handler_is_an_internal_error() {
    let world = World::new();
    let repo = world
        .add(fixtures::repository(
            "m",
            RepoFormat::Maven2,
            RepoType::Hosted,
        ))
        .await;
    let response = world.get(&repo, "a/b", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let text = body_text(response).await;
    assert!(text.contains("\"code\":\"internal\""), "{text}");
    assert!(
        !text.contains("handler"),
        "no internals in the body: {text}"
    );
}

#[tokio::test]
async fn pub_like_formats_answer_403_for_hidden_repositories_with_a_challenge() {
    let world = World::new();
    let engine = engine_with(Arc::new(PubLike), &world);
    let repo = world
        .add(fixtures::repository(
            "pubs",
            RepoFormat::Pub,
            RepoType::Hosted,
        ))
        .await;
    // Nobody has any right on the repository: a default format would answer 404.
    let head = world.head(Method::GET, "api/packages/x", &principal("dave"));
    let response = engine.serve(repo.clone(), head, Body::empty()).await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "must_not_answer_401_to_a_valid_token"
    );
    assert!(
        header_of(response.headers(), "www-authenticate")
            .unwrap()
            .starts_with("Bearer realm=\"pub\"")
    );
    assert_eq!(
        header_of(response.headers(), "content-type"),
        Some("application/vnd.pub.v2+json")
    );
    let text = body_text(response).await;
    assert!(text.contains("\"error\""), "{text}");

    // No token at all: 401 with the same challenge.
    let head = world.head(Method::GET, "api/packages/x", &anonymous());
    let response = engine.serve(repo.clone(), head, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        header_of(response.headers(), "www-authenticate"),
        Some("Bearer realm=\"pub\"")
    );

    // An authorization backend failure is a 403 for an authenticated caller, never a 401.
    world.rbac.set_failing(true);
    let head = world.head(Method::GET, "api/packages/x", &principal("dave"));
    let response = engine.serve(repo, head, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    world.rbac.set_failing(false);
}

#[tokio::test]
async fn authorization_overload_answers_503_and_a_failing_visibility_check_hides_the_repository() {
    struct Scripted {
        overloaded: bool,
    }
    #[async_trait]
    impl RbacChecker for Scripted {
        async fn check(
            &self,
            _p: &Principal,
            _a: Action,
            _r: &ResourceRef,
        ) -> Result<Decision, AuthError> {
            if self.overloaded {
                Err(AuthError::Overloaded)
            } else {
                Ok(Decision::Deny)
            }
        }
        async fn can_see_repository(
            &self,
            _p: &Principal,
            _r: &str,
            _f: RepoFormat,
        ) -> Result<bool, AuthError> {
            Err(AuthError::GroupLookupFailed("down".into()))
        }
    }
    for (overloaded, expected) in [
        (true, StatusCode::SERVICE_UNAVAILABLE),
        (false, StatusCode::NOT_FOUND),
    ] {
        let world = World::new();
        let repo = world.hosted("files").await;
        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(RawLikeHandler)).unwrap();
        let ports = Ports::from_store(
            Arc::new(world.store.clone()),
            Arc::new(Scripted { overloaded }),
            Arc::new(world.audit.clone()),
            world.clock.clone(),
            Arc::new(SingleStore(Arc::new(world.blobs.clone()))),
        );
        let engine = FormatEngine::builder(ports, registry).build().unwrap();
        let head = world.head(Method::GET, "a/b", &principal("eve"));
        let response = engine.serve(repo, head, Body::empty()).await;
        assert_eq!(response.status(), expected);
        if overloaded {
            assert!(response.headers().contains_key(header::RETRY_AFTER));
        }
    }
}

#[tokio::test]
async fn the_default_engine_builds_its_own_reqwest_client() {
    let world = World::new();
    let mut registry = FormatRegistry::new();
    registry.register(Arc::new(RawLikeHandler)).unwrap();
    let ports = Ports::from_store(
        Arc::new(world.store.clone()),
        Arc::new(FakeRbac::default()),
        Arc::new(world.audit.clone()),
        world.clock.clone(),
        Arc::new(SingleStore(Arc::new(world.blobs.clone()))),
    );
    let engine = FormatEngine::builder(ports, registry)
        .client_config(rustryx_format_core::proxy::upstream::ReqwestClientConfig::default())
        .build()
        .unwrap();
    assert_eq!(engine.registry().formats(), vec![RepoFormat::Raw]);
    assert_eq!(engine.fills_in_flight(), 0);
    assert_eq!(engine.running_tasks(), 0);
    assert!(!engine.shutdown_token().is_cancelled());
}

#[tokio::test]
async fn the_context_exposes_what_handlers_need() {
    let world = World::new();
    let repo = world.hosted("files").await;
    let mut head = world.head(Method::GET, "a/b", &principal("alice"));
    head.client.request_id = Some("req-7".into());
    let cx = world.engine.context(repo.clone(), &head);
    assert_eq!(cx.repository().name.as_str(), "files");
    assert_eq!(cx.origin_repository().name.as_str(), "files");
    assert_eq!(cx.principal().username, "alice");
    assert_eq!(cx.request_id(), Some("req-7"));
    assert_eq!(cx.limits().max_group_depth, 4);
    assert_eq!(cx.now(), cx.clock().now());
    assert!(cx.store().await.is_ok());
    assert!(!cx.shutdown_token().is_cancelled());
    assert_eq!(cx.tasks().len(), 0);
    assert!(
        cx.ports()
            .assets
            .get_by_path(repo.id, "a/b")
            .await
            .unwrap()
            .is_none()
    );
    // Pipelines refuse repository types they do not serve.
    let request = ArtifactRequest::new(
        &head,
        rustryx_format_core::NormalizedPath::parse("a/b").unwrap(),
        Action::Read,
    );
    assert!(matches!(
        cx.proxy()
            .fetch_artifact(rustryx_format_core::ProxyFetch::new(&request))
            .await,
        Err(FormatError::MethodNotAllowed { .. })
    ));
    assert!(matches!(
        cx.group().resolve(&request).await,
        Err(FormatError::Internal(_))
    ));
    let proxy = world.proxy("mirror").await;
    let proxy_cx = world.engine.context(proxy, &head);
    assert!(matches!(
        proxy_cx
            .hosted()
            .upload(
                rustryx_format_core::UploadSpec::new(
                    rustryx_format_core::NormalizedPath::parse("a").unwrap()
                ),
                Body::empty()
            )
            .await,
        Err(FormatError::MethodNotAllowed { .. })
    ));
    assert!(matches!(
        proxy_cx
            .hosted()
            .delete(&rustryx_format_core::NormalizedPath::parse("a").unwrap())
            .await,
        Err(FormatError::MethodNotAllowed { .. })
    ));
}

#[tokio::test]
async fn proxy_repository_without_proxy_settings_is_an_internal_error() {
    let world = World::new();
    let mut broken = fixtures::repository("broken", RepoFormat::Raw, RepoType::Proxy);
    broken.proxy = None;
    let repo = world.add(broken).await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "a/b.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let mut group = fixtures::repository("emptyg", RepoFormat::Raw, RepoType::Group);
    group.group = None;
    let group = world.add(group).await;
    world.allow_all("alice", &[&group]);
    assert_eq!(
        world
            .get(&group, "a/b.bin", &principal("alice"))
            .await
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn drain_waits_for_tracked_tasks() {
    let (world, repo) = {
        let world = World::new();
        let repo = world.proxy("mirror").await;
        world.allow_all("alice", &[&repo]);
        (world, repo)
    };
    world.upstream.route(
        "/d/a.bin",
        vec![
            rustryx_format_core::testing::Reply::ok(vec![1u8; 100])
                .chunked(10, Duration::from_millis(10)),
        ],
    );
    let response = world.get(&repo, "d/a.bin", &principal("alice")).await;
    drop(response); // the client leaves; the fill keeps running
    world.engine.drain().await;
    assert_eq!(world.engine.running_tasks(), 0);
    assert_eq!(
        world.blobs.len(),
        1,
        "drain returned only after the fill had finished"
    );
}

/// A format that implements nothing optional: all hooks keep their defaults.
struct DefaultsOnly;

#[async_trait]
impl FormatHandler for DefaultsOnly {
    fn formats(&self) -> &'static [RepoFormat] {
        &[RepoFormat::Raw]
    }
    fn classify(
        &self,
        repository: &Repository,
        head: &RequestHead,
    ) -> Result<ArtifactRequest, FormatError> {
        let request = classify_path_addressed(repository, head)?;
        // Every path ending in .index is an index, but this format has no merger.
        Ok(if request.path.as_str().ends_with(".index") {
            request.index()
        } else {
            request
        })
    }
    async fn handle(
        &self,
        cx: FormatContext,
        req: ArtifactRequest,
        body: Body,
    ) -> Result<Response<Body>, FormatError> {
        cx.serve_default(req, body).await
    }
}

#[tokio::test]
async fn default_hooks_give_the_generic_envelope_first_member_groups_and_hidden_repositories() {
    let world = World::new();
    let engine = engine_with(Arc::new(DefaultsOnly), &world);
    let one = world.hosted("one").await;
    let two = world.hosted("two").await;
    let group = world.group("both", &[&one, &two]).await;
    world.allow_all("alice", &[&one, &two, &group]);
    let alice = principal("alice");
    let put = |repo: Arc<Repository>, path: &'static str, data: &'static [u8]| {
        let engine = engine.clone();
        let head = world.head(Method::PUT, path, &alice);
        async move { engine.serve(repo, head, Body::from(data)).await.status() }
    };
    assert_eq!(
        put(one.clone(), "pkg/list.index", b"from one").await,
        StatusCode::CREATED
    );
    assert_eq!(
        put(two.clone(), "pkg/list.index", b"from two").await,
        StatusCode::CREATED
    );

    // No merger: the group returns the first member's index, it does not combine them.
    let head = world.head(Method::GET, "pkg/list.index", &alice);
    let response = engine.serve(group.clone(), head, Body::empty()).await;
    assert_eq!(body_text(response).await, "from one");

    // The generic envelope carries code, message and request id.
    let mut head = world.head(Method::GET, "nothing/here.bin", &alice);
    head.client.request_id = Some("req-9".into());
    let response = engine.serve(one.clone(), head, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        header_of(response.headers(), "content-type"),
        Some("application/json")
    );
    let text = body_text(response).await;
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["error"]["code"], "not_found");
    assert_eq!(json["error"]["request_id"], "req-9");

    // Hidden repository: the default format answers 404.
    let head = world.head(Method::GET, "pkg/list.index", &principal("nobody"));
    assert_eq!(
        engine.serve(one, head, Body::empty()).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn serve_default_refuses_writes_to_proxy_and_group_repositories_when_called_directly() {
    let world = World::new();
    let proxy = world.proxy("mirror").await;
    let group = world.group("both", &[&proxy]).await;
    for repo in [proxy, group] {
        let head = world.head(Method::PUT, "a/b.bin", &principal("alice"));
        let request = ArtifactRequest::new(
            &head,
            rustryx_format_core::NormalizedPath::parse("a/b.bin").unwrap(),
            Action::Write,
        );
        let cx = world.engine.context(repo, &head);
        let error = cx.serve_default(request, Body::empty()).await.unwrap_err();
        assert!(matches!(error, FormatError::MethodNotAllowed { .. }));
    }
}

#[tokio::test]
async fn formats_can_run_custom_upstream_calls_through_the_resilient_executor() {
    use futures::StreamExt;
    use rustryx_format_core::proxy::executor::{CallClass, UpstreamCall};
    use rustryx_format_core::proxy::upstream::UpstreamError;
    use rustryx_format_core::testing::Reply;

    let world = World::new();
    let repo = world.proxy("mirror").await;
    let head = world.head(Method::GET, "a", &principal("alice"));
    let cx = world.engine.context(repo.clone(), &head);
    world.upstream.route(
        "/token",
        vec![Reply::ok(&b"{\"token\":\"t\"}"[..]).header("content-type", "application/json")],
    );
    let call = |url: &str| UpstreamCall {
        repository: repo.id,
        repo_name: repo.name.to_string(),
        method: Method::GET,
        url: url::Url::parse(url).unwrap(),
        headers: http::HeaderMap::new(),
        credentials: None,
        max_concurrent: 4,
        class: CallClass::Metadata,
    };
    let result = cx
        .upstream()
        .execute(call("https://auth.example.test/token"))
        .await
        .unwrap();
    assert_eq!(result.status, StatusCode::OK);
    let mut body = result.body;
    assert_eq!(
        &body.next().await.unwrap().unwrap()[..],
        b"{\"token\":\"t\"}"
    );
    // The SSRF policy applies to custom calls too.
    let error = cx
        .upstream()
        .execute(call("https://127.0.0.1/token"))
        .await
        .unwrap_err();
    assert!(matches!(error, UpstreamError::Blocked(_)));
}
