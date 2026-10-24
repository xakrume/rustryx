//! The harness over real HTTP: hosted upload and download, authentication and authorization
//! outcomes, the anonymous gates, groups, proxies and the per format challenges.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements,
    reason = "test scenarios read best as straight-line code"
)]

mod common;

use std::sync::Arc;

use common::{client, get, header_text, put, raw_builder, raw_server};
use reqwest::StatusCode;
use rustryx_core::identity::Action;
use rustryx_core::model::{RepoFormat, WritePolicy};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use rustryx_format_core::testing::{FakeUpstream, Reply};
use rustryx_testserver::{
    NewTestRepo, READ, RawLikeHandler, TestPrivilege, TestServer, UpstreamStub,
    fast_executor_config,
};

#[tokio::test(flavor = "multi_thread")]
async fn hosted_upload_and_download_over_http() {
    let server = raw_server().await;
    let http = client();
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let url = format!("{}/docs/readme.txt", server.repository_url("files"));

    let created = put(&http, &url, Some(&alice.bearer_header()), b"hello harness").await;
    assert_eq!(created.status(), StatusCode::CREATED);

    // Bearer token, Basic with the password, and Basic with the token under any user name
    // (twine sends `__token__`) all identify the same user.
    for authorization in [
        alice.bearer_header(),
        alice.basic_header(),
        alice.token_basic_header(),
        format!("Basic {}", base64_of(&format!("__token__:{}", alice.token))),
    ] {
        let response = get(&http, &url, Some(&authorization)).await;
        assert_eq!(response.status(), StatusCode::OK, "{authorization}");
        assert_eq!(response.bytes().await.unwrap(), "hello harness");
    }

    let head = http
        .head(&url)
        .header("authorization", alice.bearer_header())
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(header_text(&head, "content-length").as_deref(), Some("13"));
    assert!(header_text(&head, "etag").is_some());

    // The upload really reached the blob store and the audit log.
    assert_eq!(server.blobs().len(), 1);
    assert!(
        server
            .audit()
            .events()
            .iter()
            .any(|event| event.action == "asset.publish")
    );
    server.shutdown().await;
}

fn base64_of(text: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(text)
}

#[tokio::test(flavor = "multi_thread")]
async fn authentication_failures_are_401_with_a_challenge_and_never_anonymous() {
    let server = raw_builder().anonymous_enabled(true).start().await;
    server
        .add_repository(NewTestRepo::hosted("open", RepoFormat::Raw).allow_anonymous(true))
        .await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let http = client();
    let url = format!("{}/a.txt", server.repository_url("open"));
    assert_eq!(
        put(&http, &url, Some(&admin.bearer_header()), b"x")
            .await
            .status(),
        StatusCode::CREATED
    );
    // Anonymous reads work on this repository.
    assert_eq!(get(&http, &url, None).await.status(), StatusCode::OK);

    // A wrong credential must NOT fall back to anonymous: a client that tried to
    // authenticate is told so.
    for wrong in [
        "Bearer ryx_not-a-real-token-at-all-0000000000000000",
        "Basic YWxpY2U6d3Jvbmc=",
        "Digest abc",
        "Bearer",
    ] {
        let response = get(&http, &url, Some(wrong)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{wrong}");
        assert_eq!(
            header_text(&response, "www-authenticate").as_deref(),
            Some(r#"Basic realm="rustryx""#)
        );
    }
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn authorization_outcomes_401_403_404() {
    let server = raw_server().await;
    let http = client();
    let admin = server.add_user("root", &["nx-admin"]).await;
    server
        .add_role_with_privileges(
            "files-readers",
            &[TestPrivilege::repository_view("files", &READ)],
        )
        .await;
    let reader = server.add_user("reader", &["files-readers"]).await;
    let nobody = server.add_user("nobody", &[]).await;
    let url = format!("{}/a.txt", server.repository_url("files"));
    put(&http, &url, Some(&admin.bearer_header()), b"data").await;

    // Missing credentials: 401 with a challenge, so maven and pip retry with credentials.
    let anonymous = get(&http, &url, None).await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert!(header_text(&anonymous, "www-authenticate").is_some());
    // The raw handler renders errors in its own shape: `render_error` ran.
    assert!(anonymous.text().await.unwrap().starts_with("RAW-ERROR"));

    // A valid user with a privilege on the repository but not for this action: 403.
    assert_eq!(
        get(&http, &url, Some(&reader.bearer_header()))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        put(&http, &url, Some(&reader.bearer_header()), b"no")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // A valid user who holds nothing on the repository must not learn that it exists: 404.
    assert_eq!(
        get(&http, &url, Some(&nobody.bearer_header()))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    // Deleting needs the delete action: the developer role has none, the admin does.
    let developer = server.add_user("dev", &["nx-developer"]).await;
    let delete = |who: String| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            http.delete(&url)
                .header("authorization", who)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(
        delete(developer.bearer_header()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(delete(admin.bearer_header()).await, StatusCode::NO_CONTENT);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_access_needs_the_switch_and_the_repository_flag() {
    let server = raw_builder().start().await;
    for (name, flag) in [("open", true), ("closed", false)] {
        server
            .add_repository(NewTestRepo::hosted(name, RepoFormat::Raw).allow_anonymous(flag))
            .await;
    }
    let admin = server.add_user("root", &["nx-admin"]).await;
    let http = client();
    for name in ["open", "closed"] {
        let url = format!("{}/a.txt", server.repository_url(name));
        put(&http, &url, Some(&admin.bearer_header()), b"data").await;
    }
    let open = format!("{}/a.txt", server.repository_url("open"));
    let closed = format!("{}/a.txt", server.repository_url("closed"));

    // Switch off (the default): nothing is readable anonymously.
    assert_eq!(
        get(&http, &open, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    server.set_anonymous_enabled(true);
    // Both gates on: readable. Flag off: still not.
    assert_eq!(get(&http, &open, None).await.status(), StatusCode::OK);
    assert_eq!(
        get(&http, &closed, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    // Anonymous callers never write, with both gates open.
    let write = put(&http, &open, None, b"overwrite").await;
    assert_eq!(write.status(), StatusCode::UNAUTHORIZED);
    // Turning the switch off refuses the very next request.
    server.set_anonymous_enabled(false);
    assert_eq!(
        get(&http, &open, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn group_fans_out_in_order_and_hides_members_the_caller_may_not_read() {
    let server = raw_builder().start().await;
    for name in ["first", "second", "secret"] {
        server
            .add_repository(NewTestRepo::hosted(name, RepoFormat::Raw))
            .await;
    }
    server
        .add_repository(NewTestRepo::group(
            "all",
            RepoFormat::Raw,
            &["first", "secret", "second"],
        ))
        .await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    server
        .add_role_with_privileges(
            "public-readers",
            &[
                TestPrivilege::repository_view("all", &READ),
                TestPrivilege::repository_view("first", &READ),
                TestPrivilege::repository_view("second", &READ),
            ],
        )
        .await;
    let reader = server.add_user("reader", &["public-readers"]).await;
    let http = client();
    let up = |repo: &'static str, path: &'static str, body: &'static [u8]| {
        let url = format!("{}/{path}", server.repository_url(repo));
        let (http, auth) = (http.clone(), admin.bearer_header());
        async move { put(&http, &url, Some(&auth), body).await.status() }
    };
    assert_eq!(
        up("second", "only-in-second.txt", b"2").await,
        StatusCode::CREATED
    );
    assert_eq!(
        up("secret", "classified.txt", b"top").await,
        StatusCode::CREATED
    );
    assert_eq!(
        up("first", "lib.index", b"one\ntwo").await,
        StatusCode::CREATED
    );
    assert_eq!(
        up("second", "lib.index", b"two\nthree").await,
        StatusCode::CREATED
    );
    assert_eq!(up("first", "__whoami", b"x").await, StatusCode::CREATED);

    let group = |path: &str| format!("{}/{path}", server.repository_url("all"));
    // A file only the last member has is found through the group.
    let found = get(
        &http,
        &group("only-in-second.txt"),
        Some(&reader.bearer_header()),
    )
    .await;
    assert_eq!(found.status(), StatusCode::OK);
    // Index documents are merged across members, in group order.
    let merged = get(&http, &group("lib.index"), Some(&reader.bearer_header())).await;
    assert_eq!(merged.text().await.unwrap(), "one\ntwo\nthree");
    // The reader may not read `secret`: through the group it is as if it did not exist.
    let hidden = get(
        &http,
        &group("classified.txt"),
        Some(&reader.bearer_header()),
    )
    .await;
    assert_eq!(hidden.status(), StatusCode::NOT_FOUND);
    let visible = get(
        &http,
        &group("classified.txt"),
        Some(&admin.bearer_header()),
    )
    .await;
    assert_eq!(visible.text().await.unwrap(), "top");
    // Groups are read only.
    let write = put(&http, &group("new.txt"), Some(&admin.bearer_header()), b"x").await;
    assert_eq!(write.status(), StatusCode::METHOD_NOT_ALLOWED);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_fetches_once_then_serves_from_the_cache() {
    let upstream = UpstreamStub::start().await;
    upstream
        .serve_file_with_headers(
            "/pkg/a.bin",
            b"upstream bytes".to_vec(),
            "application/octet-stream",
            &[("etag", "\"v1\"")],
        )
        .await;
    upstream.not_found("/pkg/missing.bin").await;
    let server = raw_builder()
        .executor_config(fast_executor_config())
        .start()
        .await;
    server
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Raw,
            &upstream.base_url(),
        ))
        .await;
    let user = server.add_user("alice", &["nx-developer"]).await;
    let http = client();
    let url = format!("{}/pkg/a.bin", server.repository_url("mirror"));

    let first = get(&http, &url, Some(&user.bearer_header())).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        header_text(&first, "x-rustryx-cache").as_deref(),
        Some("MISS")
    );
    assert_eq!(first.bytes().await.unwrap(), "upstream bytes");
    server.settle().await;
    let second = get(&http, &url, Some(&user.bearer_header())).await;
    assert_eq!(
        header_text(&second, "x-rustryx-cache").as_deref(),
        Some("HIT")
    );
    assert_eq!(upstream.count("GET", "/pkg/a.bin").await, 1);

    let missing = get(
        &http,
        &format!("{}/pkg/missing.bin", server.repository_url("mirror")),
        Some(&user.bearer_header()),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // Writes to a proxy never reach the engine's handler: 405 before authorization needs a body.
    let write = put(&http, &url, Some(&user.bearer_header()), b"x").await;
    assert_eq!(write.status(), StatusCode::METHOD_NOT_ALLOWED);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_maps_upstream_failures_and_retries_a_flaky_upstream() {
    let upstream = UpstreamStub::start().await;
    upstream.status("/broken.bin", 500).await;
    upstream.status("/denied.bin", 403).await;
    upstream
        .flaky("/flaky.bin", 2, 503, b"finally".to_vec())
        .await;
    let server = raw_builder()
        .executor_config(fast_executor_config())
        .start()
        .await;
    server
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Raw,
            &upstream.base_url(),
        ))
        .await;
    let user = server.add_user("alice", &["nx-developer"]).await;
    let http = client();
    let url = |path: &str| format!("{}/{path}", server.repository_url("mirror"));
    let auth = user.bearer_header();

    assert_eq!(
        get(&http, &url("broken.bin"), Some(&auth)).await.status(),
        StatusCode::BAD_GATEWAY
    );
    // An upstream 403 is never forwarded: the client cannot fix our credentials.
    assert_eq!(
        get(&http, &url("denied.bin"), Some(&auth)).await.status(),
        StatusCode::BAD_GATEWAY
    );
    let flaky = get(&http, &url("flaky.bin"), Some(&auth)).await;
    assert_eq!(flaky.status(), StatusCode::OK);
    assert_eq!(flaky.bytes().await.unwrap(), "finally");
    assert_eq!(upstream.count("GET", "/flaky.bin").await, 3);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_reach_an_internal_upstream_unless_the_policy_allows_it() {
    let upstream = UpstreamStub::start().await;
    upstream
        .serve_file("/a.bin", b"x".to_vec(), "text/plain")
        .await;

    // The production default denies every host: the loopback stub is not reachable.
    let strict = raw_builder()
        .upstream_policy(UpstreamUrlPolicy::default())
        .executor_config(fast_executor_config())
        .start()
        .await;
    strict
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Raw,
            &upstream.base_url(),
        ))
        .await;
    let user = strict.add_user("alice", &["nx-developer"]).await;
    let response = get(
        &client(),
        &format!("{}/a.bin", strict.repository_url("mirror")),
        Some(&user.bearer_header()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(upstream.request_count().await, 0, "nothing may connect");
    strict.shutdown().await;

    // The harness default allows loopback, but not other private ranges.
    let open = raw_builder()
        .executor_config(fast_executor_config())
        .start()
        .await;
    open.add_repository(NewTestRepo::proxy(
        "lan",
        RepoFormat::Raw,
        "http://10.255.255.1/",
    ))
    .await;
    let user = open.add_user("alice", &["nx-developer"]).await;
    let response = get(
        &client(),
        &format!("{}/a.bin", open.repository_url("lan")),
        Some(&user.bearer_header()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    open.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fake_upstream_client_replaces_http() {
    let fake = FakeUpstream::new();
    fake.route("/pkg/a.bin", vec![Reply::ok("from the fake")]);
    let server = raw_builder().upstream_client(fake.clone()).start().await;
    server
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Raw,
            "https://up.example.test/",
        ))
        .await;
    let user = server.add_user("alice", &["nx-developer"]).await;
    let response = get(
        &client(),
        &format!("{}/pkg/a.bin", server.repository_url("mirror")),
        Some(&user.bearer_header()),
    )
    .await;
    assert_eq!(response.bytes().await.unwrap(), "from the fake");
    assert_eq!(fake.calls_for("/pkg/a.bin"), 1);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn write_policy_and_scoped_tokens_are_enforced() {
    let server = raw_builder().start().await;
    server
        .add_repository(
            NewTestRepo::hosted("once", RepoFormat::Raw).write_policy(WritePolicy::AllowOnce),
        )
        .await;
    server
        .add_repository(NewTestRepo::hosted("other", RepoFormat::Raw))
        .await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let http = client();
    let url = format!("{}/v1.bin", server.repository_url("once"));
    assert_eq!(
        put(&http, &url, Some(&dev.bearer_header()), b"1")
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        put(&http, &url, Some(&dev.bearer_header()), b"2")
            .await
            .status(),
        StatusCode::CONFLICT
    );

    // A token restricted to `repo:once:read` can only narrow the developer's rights.
    let read_only = server.add_scoped_token(&dev, &["repo:once:read"]);
    let bearer = format!("Bearer {read_only}");
    assert_eq!(
        get(&http, &url, Some(&bearer)).await.status(),
        StatusCode::OK
    );
    // Within the repository the scope allows, a write is refused (403); a repository the
    // scope does not mention is not even visible to the token (404).
    let new_version = format!("{}/v2.bin", server.repository_url("once"));
    assert_eq!(
        put(&http, &new_version, Some(&bearer), b"x").await.status(),
        StatusCode::FORBIDDEN
    );
    let other = format!("{}/x.bin", server.repository_url("other"));
    assert_eq!(
        put(&http, &other, Some(&bearer), b"x").await.status(),
        StatusCode::NOT_FOUND
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn content_selectors_restrict_a_role_to_part_of_a_repository() {
    let server = raw_server().await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let selector = server
        .add_selector("public-only", r#"path.startsWith("/public/")"#)
        .await;
    server
        .add_role_with_privileges(
            "public-reader",
            &[TestPrivilege::content_selector("files", &selector, &READ)],
        )
        .await;
    let reader = server.add_user("reader", &["public-reader"]).await;
    let http = client();
    let base = server.repository_url("files");
    for path in ["public/a.txt", "private/b.txt"] {
        put(
            &http,
            &format!("{base}/{path}"),
            Some(&admin.bearer_header()),
            b"data",
        )
        .await;
    }
    let auth = reader.bearer_header();
    assert_eq!(
        get(&http, &format!("{base}/public/a.txt"), Some(&auth))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get(&http, &format!("{base}/private/b.txt"), Some(&auth))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    // A role changed after the first request is seen by the next one (the invalidation bus).
    server.set_user_roles(&reader, &[]).await;
    assert_eq!(
        get(&http, &format!("{base}/public/a.txt"), Some(&auth))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pub_answers_401_for_a_missing_or_invalid_token_and_403_for_a_valid_one_without_rights() {
    let server = TestServer::builder()
        .register(Arc::new(RawLikeHandler::for_format(RepoFormat::Pub)))
        .start()
        .await;
    server
        .add_repository(NewTestRepo::hosted("dart", RepoFormat::Pub))
        .await;
    server
        .add_role_with_privileges(
            "dart-readers",
            &[TestPrivilege::repository_view("dart", &READ)],
        )
        .await;
    let reader = server.add_user("reader", &["dart-readers"]).await;
    let nobody = server.add_user("nobody", &[]).await;
    let http = client();
    let url = format!("{}/api/packages/demo", server.repository_url("dart"));
    let challenge = r#"Bearer realm="pub""#;

    let missing = get(&http, &url, None).await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        header_text(&missing, "www-authenticate").as_deref(),
        Some(challenge)
    );
    let invalid = get(
        &http,
        &url,
        Some("Bearer ryx_invalid-token-0000000000000000000000"),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        header_text(&invalid, "www-authenticate").as_deref(),
        Some(challenge)
    );
    // Valid token, no rights: 403, which pub clients answer by keeping the stored token.
    let forbidden = get(&http, &url, Some(&nobody.bearer_header())).await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        header_text(&forbidden, "www-authenticate").as_deref(),
        Some(challenge)
    );
    // Valid token with rights: no challenge.
    let allowed = get(&http, &url, Some(&reader.bearer_header())).await;
    assert_eq!(allowed.status(), StatusCode::NOT_FOUND);
    assert!(header_text(&allowed, "www-authenticate").is_none());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_urls_and_ids() {
    let server = raw_server().await;
    let http = client();
    assert_eq!(
        server.repository_url("files"),
        format!("{}/repository/files", server.base_url())
    );
    assert!(server.base_url().starts_with("http://127.0.0.1:"));
    // Loopback only: a container cannot reach it, so the host URL is the plain one.
    assert_eq!(server.host_base_url(), server.base_url());
    assert_eq!(
        server.host_repository_url("files"),
        server.repository_url("files")
    );

    let unknown_route = http
        .get(format!("{}/nothing", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_route.status(), StatusCode::NOT_FOUND);
    assert!(header_text(&unknown_route, "x-request-id").is_some());
    for path in ["no-such-repo/a.txt", "bad%20name/a.txt"] {
        let response = http
            .get(format!("{}/repository/{path}", server.base_url()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    // The raw path reaches `classify`: an encoded slash is a bad request, not a lookup.
    let admin = server.add_user("root", &["nx-admin"]).await;
    let encoded = get(
        &http,
        &format!("{}/a%2fb", server.repository_url("files")),
        Some(&admin.bearer_header()),
    )
    .await;
    assert_eq!(encoded.status(), StatusCode::BAD_REQUEST);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_all_makes_the_docker_host_url() {
    let server = raw_builder().bind_all(true).start().await;
    assert!(
        server
            .host_base_url()
            .starts_with("http://host.docker.internal:")
    );
    assert!(server.base_url().starts_with("http://127.0.0.1:"));
    assert!(
        server
            .host_repository_url("files")
            .ends_with("/repository/files")
    );
    // Still reachable on loopback.
    let response = client()
        .get(format!("{}/nothing", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fixtures_report_their_own_mistakes() {
    let server = raw_server().await;
    assert!(
        server
            .try_add_repository(NewTestRepo::hosted("files", RepoFormat::Raw))
            .await
            .is_err(),
        "duplicate name"
    );
    let invalid = server
        .try_add_repository(NewTestRepo::hosted("not valid!", RepoFormat::Raw))
        .await
        .unwrap_err();
    assert!(invalid.to_string().contains("invalid repository name"));
    let group = server
        .try_add_repository(NewTestRepo::group("g", RepoFormat::Raw, &["bad name"]))
        .await;
    assert!(group.is_err());
    let unknown_role = server
        .try_add_user("u", &["no-such-role"])
        .await
        .unwrap_err();
    assert!(unknown_role.to_string().contains("no-such-role"));
    let bad_selector = server
        .try_add_selector("s", "this is not cel")
        .await
        .unwrap_err();
    assert!(bad_selector.to_string().contains("selector"));
    server.add_user("dup", &[]).await;
    assert!(server.try_add_user("dup", &[]).await.is_err());
    server.add_role_with_privileges("r", &[]).await;
    assert!(server.try_add_role_with_privileges("r", &[]).await.is_err());

    let user = server.add_user("secretive", &[]).await;
    let shown = format!("{user:?}");
    assert!(!shown.contains(&user.password) && !shown.contains(&user.token));
    assert!(shown.contains("[REDACTED]"));
    assert!(user.token.starts_with("ryx_"));
    let _ = (
        server.store(),
        server.rbac(),
        server.engine().running_tasks(),
        server.port(),
        Action::Read,
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_the_listener() {
    let server = raw_server().await;
    let url = format!("{}/nothing", server.base_url());
    let http = client();
    assert_eq!(
        http.get(&url).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    server.shutdown().await;
    assert!(
        http.get(&url).send().await.is_err(),
        "the port must be closed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_offline_repository_answers_503_and_formats_cannot_be_registered_twice() {
    let server = raw_server().await;
    server
        .add_repository(
            NewTestRepo::hosted("down", RepoFormat::Raw)
                .online(false)
                .quota_bytes(10),
        )
        .await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let response = get(
        &client(),
        &format!("{}/a.txt", server.repository_url("down")),
        Some(&admin.bearer_header()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    server.shutdown().await;

    let duplicate = TestServer::builder()
        .register(Arc::new(RawLikeHandler::new()))
        .register(Arc::new(RawLikeHandler::new()))
        .try_start()
        .await;
    assert!(duplicate.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_store_is_503_and_never_anonymous_or_roleless() {
    let server = raw_server().await;
    let admin = server.add_user("root", &["nx-admin"]).await;
    let http = client();
    let url = format!("{}/a.txt", server.repository_url("files"));
    server.store().set_unavailable(true);

    // Even the repository lookup fails: that is a 503, not a 404 and not an anonymous answer.
    for authorization in [None, Some(admin.bearer_header())] {
        let response = get(&http, &url, authorization.as_deref()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header_text(&response, "retry-after").as_deref(), Some("1"));
    }
    server.store().set_unavailable(false);

    // With the repository found but the roles unreadable, a valid token is still not
    // treated as "no roles": this is the path the authenticator takes on its own. It is
    // reached by failing the store only after the repository lookup would have succeeded,
    // which the in-memory store cannot split, so the OCI endpoints stand in for it.
    server.store().set_unavailable(true);
    let ping = get(
        &http,
        &format!("{}/v2/", server.base_url()),
        Some(&admin.bearer_header()),
    )
    .await;
    assert_eq!(ping.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header_text(&ping, "retry-after").as_deref(), Some("1"));
    let token = get(
        &http,
        &format!("{}/v2/token", server.base_url()),
        Some(&admin.basic_header()),
    )
    .await;
    assert_eq!(token.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header_text(&token, "retry-after").as_deref(), Some("1"));
    let oci = get(
        &http,
        &format!("{}/v2/files/app/manifests/1", server.base_url()),
        None,
    )
    .await;
    assert_eq!(oci.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header_text(&oci, "retry-after").as_deref(), Some("1"));
    server.store().set_unavailable(false);
    assert_eq!(
        get(&http, &url, Some(&admin.bearer_header()))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    server.shutdown().await;
}
