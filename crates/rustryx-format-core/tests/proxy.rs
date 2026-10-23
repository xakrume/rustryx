//! Proxy pipeline: cache fill, singleflight, tee, stale, negative cache, checksums.
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
use std::time::{Duration, Instant};

use axum_core::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderValue, Method, StatusCode, header};
use rustryx_core::Secret;
use rustryx_core::model::{BlobRecord, NewAsset, ProxyConfig, Repository, UpstreamAuth};
use rustryx_core::ports::{AssetRepo, BlobKey};
use rustryx_format_core::testing::Reply;
use rustryx_format_core::{
    ArtifactRequest, ContentEvent, ExpectedChecksum, FormatError, HashAlgo, Limits, NormalizedPath,
    ProxyFetch, StandardChecksumHeaders, UpstreamFailureKind,
};
use rustryx_testkit::fixtures;
use support::{World, body_bytes, body_text, eventually, header_of, principal};

const CONTENT: &[u8] = b"proxied artifact bytes";

fn reconfigure(repo: &Repository, change: impl FnOnce(&mut ProxyConfig)) -> Repository {
    let mut repo = repo.clone();
    change(repo.proxy.as_mut().unwrap());
    repo
}

async fn proxy_world() -> (World, Arc<Repository>) {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    (world, repo)
}

fn request_for(
    world: &World,
    repo: &Arc<Repository>,
    path: &str,
) -> (ArtifactRequest, rustryx_format_core::FormatContext) {
    let head = world.head(Method::GET, path, &principal("alice"));
    let request = ArtifactRequest::new(
        &head,
        NormalizedPath::parse(path).unwrap(),
        rustryx_core::identity::Action::Read,
    );
    (request, world.engine.context(repo.clone(), &head))
}

#[tokio::test]
async fn miss_streams_the_body_and_caches_it_for_the_next_request() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/a/b.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");

    let response = world.get(&repo, "a/b.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("MISS")
    );
    assert_eq!(header_of(response.headers(), "content-length"), Some("22"));
    assert_eq!(
        header_of(response.headers(), "x-content-type-options"),
        Some("nosniff")
    );
    assert_eq!(&body_bytes(response).await[..], CONTENT);

    world.settle().await;
    assert_eq!(world.blobs.len(), 1, "the fill committed one blob");
    assert_eq!(world.blobs.staging_count(), 0);

    let response = world.get(&repo, "a/b.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT")
    );
    let etag = header_of(response.headers(), "etag").unwrap().to_owned();
    assert_eq!(
        etag,
        format!("\"{}\"", fixtures::checksums_of(CONTENT).sha256)
    );
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    assert_eq!(
        world.upstream.calls(),
        1,
        "the second request is a cache hit"
    );
}

#[tokio::test]
async fn must_not_fetch_upstream_n_times_for_n_concurrent_misses() {
    let (world, repo) = proxy_world().await;
    // Slow enough that all requests pile up while the first fill runs.
    world.upstream.route(
        "/big/x.bin",
        vec![Reply::ok(vec![7u8; 4096]).chunked(512, Duration::from_millis(15))],
    );
    let alice = principal("alice");
    let mut tasks = Vec::new();
    for _ in 0..50 {
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::GET, "big/x.bin", &alice);
        tasks.push(tokio::spawn(async move {
            let response = engine.serve(repo, head, Body::empty()).await;
            let status = response.status();
            (status, body_bytes(response).await)
        }));
    }
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.len(), 4096);
        assert!(body.iter().all(|b| *b == 7));
    }
    assert_eq!(
        world.upstream.calls(),
        1,
        "50 concurrent misses must cause exactly one upstream request"
    );
    world.settle().await;
    assert_eq!(
        world.engine.fills_in_flight(),
        0,
        "the key must be removed on success"
    );
    assert_eq!(world.blobs.len(), 1);
}

#[tokio::test]
async fn followers_get_the_failure_and_the_key_is_removed_on_error() {
    let (world, repo) = proxy_world().await;
    // 400 is not retried, so the single upstream call is observable.
    world.upstream.route(
        "/bad.bin",
        vec![Reply::status(400).delay_head(Duration::from_millis(80))],
    );
    let alice = principal("alice");
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::GET, "bad.bin", &alice);
        tasks.push(tokio::spawn(async move {
            engine.serve(repo, head, Body::empty()).await.status()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::BAD_GATEWAY);
    }
    assert_eq!(world.upstream.calls(), 1);
    world.settle().await;
    assert_eq!(
        world.engine.fills_in_flight(),
        0,
        "the key must be removed on error"
    );
    // A later request starts a new fill.
    world.upstream.route("/bad.bin", vec![Reply::ok(CONTENT)]);
    assert_eq!(
        world.get(&repo, "bad.bin", &alice).await.status(),
        StatusCode::OK
    );
    assert_eq!(world.upstream.calls(), 2);
}

#[tokio::test]
async fn must_not_cancel_the_fill_when_the_leading_client_disconnects_before_the_head() {
    let (world, repo) = proxy_world().await;
    world.upstream.route(
        "/slow/a.bin",
        vec![
            Reply::ok(CONTENT)
                .delay_head(Duration::from_millis(100))
                .chunked(8, Duration::from_millis(5)),
        ],
    );
    let alice = principal("alice");
    let leader = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::GET, "slow/a.bin", &alice);
        async move { engine.serve(repo, head, Body::empty()).await.status() }
    });
    eventually("fill started", || world.upstream.calls() == 1).await;
    leader.abort();
    let _ = leader.await;

    world.settle().await;
    assert_eq!(world.upstream.calls(), 1, "no second upstream request");
    assert_eq!(
        world.blobs.len(),
        1,
        "the fill finished although its client left"
    );
    let response = world.get(&repo, "slow/a.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT")
    );
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    assert_eq!(world.upstream.calls(), 1);
}

#[tokio::test]
async fn must_not_cancel_the_fill_when_the_client_drops_the_body_midway() {
    let (world, repo) = proxy_world().await;
    let data = vec![9u8; 64 * 1024];
    world.upstream.route(
        "/half/a.bin",
        vec![Reply::ok(data.clone()).chunked(4096, Duration::from_millis(2))],
    );
    let alice = principal("alice");
    let response = world.get(&repo, "half/a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let first = stream.next().await.unwrap().unwrap();
    assert!(!first.is_empty());
    drop(stream); // the client closes the connection after the first chunk

    world.settle().await;
    assert_eq!(
        world.blobs.len(),
        1,
        "the cache fill continued in its own task"
    );
    let response = world.get(&repo, "half/a.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT")
    );
    assert_eq!(body_bytes(response).await.len(), data.len());
    assert_eq!(world.upstream.calls(), 1);
}

#[tokio::test]
async fn must_not_replace_an_artifact_that_was_published_while_the_fill_ran() {
    let (world, repo) = proxy_world().await;
    world.upstream.route(
        "/race/a.bin",
        vec![Reply::ok(CONTENT).delay_head(Duration::from_millis(120))],
    );
    let alice = principal("alice");
    let request = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::GET, "race/a.bin", &alice);
        async move { body_bytes(engine.serve(repo, head, Body::empty()).await).await }
    });
    eventually("fill started", || world.upstream.calls() == 1).await;
    // Someone else (another node) publishes the artifact first.
    let key = "11111111-1111-7111-8111-111111111111";
    let published = fixtures::blob_record(key, b"published by someone else");
    world.blobs.put(
        &BlobKey::new(key),
        Bytes::from_static(b"published by someone else"),
    );
    world
        .store
        .publish(&NewAsset {
            repository_id: repo.id,
            component_id: None,
            path: "race/a.bin".into(),
            content_type: "application/octet-stream".into(),
            blob: BlobRecord {
                blob_store_id: repo.blob_store_id,
                ..published
            },
        })
        .await
        .unwrap();
    let _ = request.await.unwrap();
    world.settle().await;
    let asset = world
        .store
        .get_by_path(repo.id, "race/a.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        asset.size_bytes, 25,
        "the fill must not replace the published artifact"
    );
    assert_eq!(
        world.blobs.len(),
        1,
        "the fill's own blob is deleted, only the published one remains"
    );
}

#[tokio::test]
async fn upstream_404_is_cached_negatively() {
    let (world, repo) = proxy_world().await;
    world
        .upstream
        .route("/none/a.bin", vec![Reply::status(404)]);
    let alice = principal("alice");
    assert_eq!(
        world.get(&repo, "none/a.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.get(&repo, "none/a.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.upstream.calls(),
        1,
        "the second 404 comes from the negative cache"
    );
    // A different path is unaffected.
    world.upstream.route("/other.bin", vec![Reply::ok(CONTENT)]);
    assert_eq!(
        world.get(&repo, "other.bin", &alice).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn negative_cache_entries_expire() {
    let world = World::new();
    let base = fixtures::repository(
        "short",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Proxy,
    );
    let repo = world
        .add(reconfigure(&base, |c| c.negative_cache_ttl_secs = 1))
        .await;
    world.allow_all("alice", &[&repo]);
    world
        .upstream
        .route("/x.bin", vec![Reply::status(404), Reply::ok(CONTENT)]);
    let alice = principal("alice");
    assert_eq!(
        world.get(&repo, "x.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(
        world.get(&repo, "x.bin", &alice).await.status(),
        StatusCode::OK
    );
    assert_eq!(world.upstream.calls(), 2);
}

#[tokio::test]
async fn must_not_forward_upstream_401_or_403_to_the_client() {
    let (world, repo) = proxy_world().await;
    let alice = principal("alice");
    for code in [401u16, 403] {
        world
            .upstream
            .route(&format!("/auth{code}.bin"), vec![Reply::status(code)]);
        let response = world.get(&repo, &format!("auth{code}.bin"), &alice).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_GATEWAY,
            "upstream {code}"
        );
        assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
        assert_eq!(
            world.upstream.calls_for(&format!("/auth{code}.bin")),
            1,
            "an auth failure is not retried"
        );
    }
}

#[tokio::test]
async fn forbidden_can_be_treated_as_a_miss_when_the_format_says_so() {
    let (world, repo) = proxy_world().await;
    world
        .upstream
        .route("/s3style.bin", vec![Reply::status(403)]);
    let (request, cx) = request_for(&world, &repo, "s3style.bin");
    let error = cx
        .proxy()
        .fetch_artifact(ProxyFetch::new(&request).forbidden_is_miss())
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::NotFound));
    // A 401 stays an upstream failure even then.
    world
        .upstream
        .route("/s3style401.bin", vec![Reply::status(401)]);
    let (request, cx) = request_for(&world, &repo, "s3style401.bin");
    let error = cx
        .proxy()
        .fetch_artifact(ProxyFetch::new(&request).forbidden_is_miss())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        FormatError::Upstream {
            kind: UpstreamFailureKind::Rejected,
            ..
        }
    ));
}

#[tokio::test]
async fn upstream_5xx_is_retried_three_times_then_502() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/err.bin", vec![Reply::status(503)]);
    let response = world.get(&repo, "err.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(world.upstream.calls(), 3);
    assert_eq!(body_text(response).await, "RAW-ERROR upstream_error");
}

#[tokio::test]
async fn a_retry_that_succeeds_serves_the_artifact() {
    let (world, repo) = proxy_world().await;
    world
        .upstream
        .route("/flaky.bin", vec![Reply::status(502), Reply::ok(CONTENT)]);
    let response = world.get(&repo, "flaky.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    assert_eq!(world.upstream.calls(), 2);
}

#[tokio::test]
async fn upstream_timeout_is_504() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/hang.bin", vec![Reply::hang()]);
    let started = Instant::now();
    let response = world.get(&repo, "hang.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(5));
    world.settle().await;
}

#[tokio::test]
async fn serves_a_stale_copy_with_a_marker_when_the_upstream_is_down() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/s/a.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");
    body_bytes(world.get(&repo, "s/a.bin", &alice).await).await;
    world.settle().await;

    world.clock.advance(Duration::from_secs(2 * 3600)); // beyond content_max_age (3600 s)
    world.upstream.route("/s/a.bin", vec![Reply::status(503)]);
    let response = world.get(&repo, "s/a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("STALE")
    );
    assert!(
        header_of(response.headers(), "warning")
            .unwrap()
            .starts_with("110 ")
    );
    assert_eq!(&body_bytes(response).await[..], CONTENT);
}

#[tokio::test]
async fn a_stale_copy_older_than_the_limit_is_not_served() {
    let limits = Limits {
        stale_max_age: Duration::from_secs(5 * 3600),
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    world.upstream.route("/s/a.bin", vec![Reply::ok(CONTENT)]);
    body_bytes(world.get(&repo, "s/a.bin", &alice).await).await;
    world.settle().await;
    world.clock.advance(Duration::from_secs(6 * 3600));
    world.upstream.route("/s/a.bin", vec![Reply::status(500)]);
    assert_eq!(
        world.get(&repo, "s/a.bin", &alice).await.status(),
        StatusCode::BAD_GATEWAY
    );
}

#[tokio::test]
async fn an_expired_copy_is_refreshed_and_the_old_blob_is_announced() {
    let (world, repo) = proxy_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/r/a.bin",
        vec![
            Reply::ok(&b"version one"[..]),
            Reply::ok(&b"version two!"[..]),
        ],
    );
    body_bytes(world.get(&repo, "r/a.bin", &alice).await).await;
    world.settle().await;
    world.clock.advance(Duration::from_secs(2 * 3600));
    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("MISS")
    );
    assert_eq!(&body_bytes(response).await[..], b"version two!");
    world.settle().await;
    assert_eq!(world.upstream.calls(), 2);
    assert_eq!(world.events.orphaned(), 1);
    let asset = world
        .store
        .get_by_path(repo.id, "r/a.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(asset.size_bytes, 12);
}

#[tokio::test]
async fn an_expired_copy_survives_when_the_refresh_gets_404_but_the_client_sees_404() {
    let (world, repo) = proxy_world().await;
    let alice = principal("alice");
    world
        .upstream
        .route("/gone.bin", vec![Reply::ok(CONTENT), Reply::status(404)]);
    body_bytes(world.get(&repo, "gone.bin", &alice).await).await;
    world.settle().await;
    world.clock.advance(Duration::from_secs(2 * 3600));
    assert_eq!(
        world.get(&repo, "gone.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn verified_fetch_serves_only_after_the_checksum_matches() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/v/a.bin", vec![Reply::ok(CONTENT)]);
    let sha = fixtures::checksums_of(CONTENT).sha256;
    let (request, cx) = request_for(&world, &repo, "v/a.bin");
    let fetch =
        ProxyFetch::new(&request).expect(ExpectedChecksum::new(HashAlgo::Sha256, &sha).unwrap());
    let response = cx.proxy().fetch_artifact(fetch).await.unwrap();
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("MISS")
    );
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    assert_eq!(world.blobs.len(), 1);
}

#[tokio::test]
async fn must_not_serve_or_cache_content_with_a_wrong_checksum() {
    let (world, repo) = proxy_world().await;
    world
        .upstream
        .route("/v/evil.bin", vec![Reply::ok(&b"tampered bytes"[..])]);
    let (request, cx) = request_for(&world, &repo, "v/evil.bin");
    let fetch = ProxyFetch::new(&request).expect(
        ExpectedChecksum::new(HashAlgo::Sha256, &fixtures::checksums_of(CONTENT).sha256).unwrap(),
    );
    let error = cx.proxy().fetch_artifact(fetch).await.unwrap_err();
    assert!(
        matches!(
            error,
            FormatError::Upstream {
                kind: UpstreamFailureKind::Integrity,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        world.blobs.len(),
        0,
        "a corrupt artifact must not stay in the store"
    );
    assert!(
        world
            .store
            .get_by_path(repo.id, "v/evil.bin")
            .await
            .unwrap()
            .is_none()
    );
    let events = world.audit.events();
    assert!(
        events
            .iter()
            .any(|e| e.action == "proxy.checksum_mismatch" && e.outcome == "failure")
    );
}

#[tokio::test]
async fn a_checksum_announced_in_response_headers_is_verified_too() {
    let (world, repo) = proxy_world().await;
    let good = fixtures::checksums_of(CONTENT).sha1;
    world.upstream.route(
        "/h/ok.bin",
        vec![Reply::ok(CONTENT).header("x-checksum-sha1", &good)],
    );
    world.upstream.route(
        "/h/bad.bin",
        vec![Reply::ok(CONTENT).header("x-checksum-sha1", &"0".repeat(40))],
    );
    for (path, ok) in [("h/ok.bin", true), ("h/bad.bin", false)] {
        let (request, cx) = request_for(&world, &repo, path);
        let fetch = ProxyFetch::new(&request).checksum_source(Arc::new(StandardChecksumHeaders));
        let result = cx.proxy().fetch_artifact(fetch).await;
        assert_eq!(result.is_ok(), ok, "{path}");
    }
    assert_eq!(world.blobs.len(), 1);
}

#[tokio::test]
async fn tee_answers_before_the_body_is_complete_but_verification_does_not() {
    let (world, repo) = proxy_world().await;
    let body = vec![1u8; 2400];
    let slow = || Reply::ok(body.clone()).chunked(400, Duration::from_millis(60)); // about 360 ms
    world.upstream.route("/t/tee.bin", vec![slow()]);
    world.upstream.route("/t/verified.bin", vec![slow()]);

    let (request, cx) = request_for(&world, &repo, "t/tee.bin");
    let started = Instant::now();
    let response = cx
        .proxy()
        .fetch_artifact(ProxyFetch::new(&request))
        .await
        .unwrap();
    let head_after = started.elapsed();
    assert!(
        head_after < Duration::from_millis(250),
        "tee head after {head_after:?}"
    );
    assert_eq!(body_bytes(response).await.len(), 2400);

    let (request, cx) = request_for(&world, &repo, "t/verified.bin");
    let sha = fixtures::checksums_of(&body).sha256;
    let started = Instant::now();
    let fetch =
        ProxyFetch::new(&request).expect(ExpectedChecksum::new(HashAlgo::Sha256, &sha).unwrap());
    let response = cx.proxy().fetch_artifact(fetch).await.unwrap();
    let verified_after = started.elapsed();
    assert!(
        verified_after >= Duration::from_millis(300),
        "nothing is served before verification: {verified_after:?}"
    );
    assert_eq!(body_bytes(response).await.len(), 2400);
}

#[tokio::test]
async fn must_not_cache_a_truncated_body_and_the_client_stream_breaks() {
    let (world, repo) = proxy_world().await;
    world.upstream.route(
        "/trunc.bin",
        vec![Reply::ok(&b"abcd"[..]).header("content-length", "10")],
    );
    let response = world.get(&repo, "trunc.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let result = rustryx_format_core::stream::read_bounded(response.into_body(), 100).await;
    assert!(
        result.is_err(),
        "a truncated download must fail, not end cleanly"
    );
    world.settle().await;
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
    assert!(
        world
            .store
            .get_by_path(repo.id, "trunc.bin")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_connection_that_breaks_mid_body_aborts_the_cache_entry() {
    let (world, repo) = proxy_world().await;
    world.upstream.route(
        "/mid.bin",
        vec![
            Reply::ok(vec![5u8; 100])
                .chunked(10, Duration::ZERO)
                .fail_after_chunks(3),
        ],
    );
    let response = world.get(&repo, "mid.bin", &principal("alice")).await;
    assert!(
        rustryx_format_core::stream::read_bounded(response.into_body(), 1000)
            .await
            .is_err()
    );
    world.settle().await;
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
}

#[tokio::test]
async fn a_stalled_upstream_stream_is_cut_by_the_idle_timeout() {
    let (world, repo) = proxy_world().await;
    // 100 bytes in 10 chunks, 300 ms apart: longer than the 250 ms idle window.
    world.upstream.route(
        "/idle.bin",
        vec![Reply::ok(vec![1u8; 100]).chunked(10, Duration::from_millis(300))],
    );
    let response = world.get(&repo, "idle.bin", &principal("alice")).await;
    // The head is sent after the first chunk? No: after the response head. The body then breaks.
    assert!(
        rustryx_format_core::stream::read_bounded(response.into_body(), 1000)
            .await
            .is_err()
    );
    world.settle().await;
    assert_eq!(world.blobs.len(), 0);
}

#[tokio::test]
async fn oversized_artifacts_are_passed_through_uncached_in_tee_mode() {
    let limits = Limits {
        max_blob_bytes: 10,
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    world
        .upstream
        .route("/big.bin", vec![Reply::ok(vec![3u8; 50])]);
    let response = world.get(&repo, "big.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_bytes(response).await.len(),
        50,
        "the client still gets its download"
    );
    world.settle().await;
    assert_eq!(world.blobs.len(), 0, "but it is not cached");
    assert_eq!(world.blobs.staging_count(), 0);
}

#[tokio::test]
async fn repository_quota_degrades_the_proxy_to_pass_through() {
    let world = World::new();
    let mut repo = fixtures::repository(
        "tiny",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Proxy,
    );
    repo.quota_bytes = Some(5);
    let repo = world.add(repo).await;
    world.allow_all("alice", &[&repo]);
    world.upstream.route("/q.bin", vec![Reply::ok(CONTENT)]);
    let response = world.get(&repo, "q.bin", &principal("alice")).await;
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    world.settle().await;
    assert_eq!(world.blobs.len(), 0);
}

#[tokio::test]
async fn an_unwritable_blob_store_does_not_break_the_download() {
    let (world, repo) = proxy_world().await;
    world.blobs.set_unavailable(true);
    world.upstream.route("/ns.bin", vec![Reply::ok(CONTENT)]);
    let response = world.get(&repo, "ns.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    world.blobs.set_unavailable(false);
}

#[tokio::test]
async fn head_miss_goes_upstream_as_head_and_caches_nothing() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/hd/a.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");
    let response = world
        .send(&repo, Method::HEAD, "hd/a.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_of(response.headers(), "content-length"), Some("22"));
    assert!(body_bytes(response).await.is_empty());
    let requests = world.upstream.requests();
    assert_eq!(requests[0].method, Method::HEAD);
    assert_eq!(
        world.blobs.len(),
        0,
        "a HEAD must not download the artifact"
    );

    world
        .upstream
        .route("/hd/missing.bin", vec![Reply::status(404)]);
    let response = world
        .send(&repo, Method::HEAD, "hd/missing.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Once cached, HEAD is answered locally.
    body_bytes(world.get(&repo, "hd/a.bin", &alice).await).await;
    world.settle().await;
    let before = world.upstream.calls();
    let response = world
        .send(&repo, Method::HEAD, "hd/a.bin", &alice, Body::empty())
        .await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT")
    );
    assert_eq!(world.upstream.calls(), before);
}

#[tokio::test]
async fn range_is_ignored_on_a_miss_and_honoured_on_a_hit() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/rg/a.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");
    let ranged = || {
        let mut head = world.head(Method::GET, "rg/a.bin", &alice);
        head.headers
            .insert(header::RANGE, HeaderValue::from_static("bytes=0-3"));
        head
    };
    let response = world.send_with(&repo, ranged(), Body::empty()).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a miss answers the whole body"
    );
    assert_eq!(body_bytes(response).await.len(), 22);
    world.settle().await;
    let response = world.send_with(&repo, ranged(), Body::empty()).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body_bytes(response).await[..], b"prox");
}

#[tokio::test]
async fn index_documents_use_the_metadata_ttl_and_are_buffered_in_memory() {
    let (world, repo) = proxy_world().await;
    world.upstream.route(
        "/pkg/list.index",
        vec![
            Reply::ok(&b"one\ntwo"[..]),
            Reply::ok(&b"one\ntwo\nthree"[..]),
        ],
    );
    let alice = principal("alice");
    let response = world.get(&repo, "pkg/list.index", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("MISS")
    );
    assert_eq!(&body_bytes(response).await[..], b"one\ntwo");
    let response = world.get(&repo, "pkg/list.index", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT")
    );
    assert!(response.headers().contains_key("etag"));
    // metadata_max_age is 600 s, content_max_age 3600 s: after 700 s only the index is stale.
    world.clock.advance(Duration::from_secs(700));
    let response = world.get(&repo, "pkg/list.index", &alice).await;
    assert_eq!(&body_bytes(response).await[..], b"one\ntwo\nthree");
    assert_eq!(world.upstream.calls(), 2);
}

#[tokio::test]
async fn an_expired_index_is_served_stale_when_the_upstream_fails() {
    let (world, repo) = proxy_world().await;
    world
        .upstream
        .route("/pkg/list.index", vec![Reply::ok(&b"one"[..])]);
    let alice = principal("alice");
    body_bytes(world.get(&repo, "pkg/list.index", &alice).await).await;
    world.clock.advance(Duration::from_secs(700));
    world
        .upstream
        .route("/pkg/list.index", vec![Reply::status(502)]);
    let response = world.get(&repo, "pkg/list.index", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("STALE")
    );
    assert!(response.headers().contains_key("warning"));
    assert_eq!(&body_bytes(response).await[..], b"one");
}

#[tokio::test]
async fn an_oversized_index_document_is_refused() {
    let limits = Limits {
        max_document_bytes: 8,
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    world
        .upstream
        .route("/huge.index", vec![Reply::ok(vec![b'x'; 64])]);
    let response = world.get(&repo, "huge.index", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(world.blobs.len(), 0);
}

#[tokio::test]
async fn must_not_forward_client_headers_to_the_upstream() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/fw/a.bin", vec![Reply::ok(CONTENT)]);
    let mut head = world.head(Method::GET, "fw/a.bin", &principal("alice"));
    head.headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer ryx_client_secret"),
    );
    head.headers
        .insert(header::COOKIE, HeaderValue::from_static("session=abc"));
    head.headers
        .insert("x-forwarded-for", HeaderValue::from_static("10.9.9.9"));
    body_bytes(world.send_with(&repo, head, Body::empty()).await).await;
    let sent = &world.upstream.requests()[0].headers;
    assert!(
        sent.is_empty(),
        "nothing of the client's request may reach the upstream: {sent:?}"
    );
}

#[tokio::test]
async fn repository_credentials_are_sent_as_sensitive_basic_auth() {
    let world = World::new();
    let base = fixtures::repository(
        "authd",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Proxy,
    );
    let repo = world
        .add(reconfigure(&base, |c| {
            c.auth = Some(UpstreamAuth {
                username: "svc".into(),
                password: Secret::new("s3cret"),
            });
        }))
        .await;
    world.allow_all("alice", &[&repo]);
    world.upstream.route("/c/a.bin", vec![Reply::ok(CONTENT)]);
    body_bytes(world.get(&repo, "c/a.bin", &principal("alice")).await).await;
    let sent = &world.upstream.requests()[0].headers;
    let auth = sent.get(header::AUTHORIZATION).unwrap();
    assert!(auth.is_sensitive());
    use base64::Engine as _;
    assert_eq!(
        auth.to_str().unwrap(),
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("svc:s3cret")
        )
    );
}

#[tokio::test]
async fn accept_and_cache_path_overrides_are_honoured() {
    let (world, repo) = proxy_world().await;
    world
        .upstream
        .route("/api/pkg", vec![Reply::ok(&b"{\"v\":2}"[..])]);
    let (request, cx) = request_for(&world, &repo, "pkg");
    let fetch = ProxyFetch::new(&request)
        .upstream_path(rustryx_format_core::UpstreamPath::Decoded(
            NormalizedPath::parse("api/pkg").unwrap(),
        ))
        .accept(HeaderValue::from_static("application/vnd.pub.v2+json"))
        .cache_path("api/pkg.v2json")
        .content_type("application/vnd.pub.v2+json")
        .header(
            http::HeaderName::from_static("x-extra"),
            HeaderValue::from_static("1"),
        );
    let document = cx.proxy().fetch_document(fetch).await.unwrap();
    assert_eq!(&document.body[..], b"{\"v\":2}");
    let sent = &world.upstream.requests()[0];
    assert_eq!(
        header_of(&sent.headers, "accept"),
        Some("application/vnd.pub.v2+json")
    );
    assert_eq!(header_of(&sent.headers, "x-extra"), Some("1"));
    let asset = world
        .store
        .get_by_path(repo.id, "api/pkg.v2json")
        .await
        .unwrap()
        .unwrap();
    assert!(asset.content_type.starts_with("application/"));
    let headers = document.response_headers();
    assert!(headers.contains_key("etag"));
    assert_eq!(header_of(&headers, "x-rustryx-cache"), Some("MISS"));
}

#[tokio::test]
async fn credentials_set_by_the_format_are_sent_and_marked_sensitive() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/tok/a.bin", vec![Reply::ok(CONTENT)]);
    let (request, cx) = request_for(&world, &repo, "tok/a.bin");
    let fetch = ProxyFetch::new(&request).header(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer registry-token"),
    );
    cx.proxy().fetch_artifact(fetch).await.unwrap();
    let sent = &world.upstream.requests()[0].headers;
    let value = sent.get(header::AUTHORIZATION).unwrap();
    assert_eq!(value, "Bearer registry-token");
    assert!(
        value.is_sensitive(),
        "must_not_leak the token through Debug output"
    );
}

#[tokio::test]
async fn the_circuit_breaker_stops_calls_to_a_failing_upstream() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/cb/a.bin", vec![Reply::status(500)]);
    let alice = principal("alice");
    // min_requests is 4 in the test harness: two requests of three attempts open it.
    for _ in 0..2 {
        assert_eq!(
            world.get(&repo, "cb/a.bin", &alice).await.status(),
            StatusCode::BAD_GATEWAY
        );
    }
    let before = world.upstream.calls();
    let response = world.get(&repo, "cb/b.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        world.upstream.calls(),
        before,
        "an open breaker must not reach the upstream"
    );
    assert_eq!(body_text(response).await, "RAW-ERROR upstream_circuit_open");
}

#[tokio::test]
async fn the_circuit_breaker_serves_stale_copies() {
    let (world, repo) = proxy_world().await;
    let alice = principal("alice");
    world
        .upstream
        .route("/cb/keep.bin", vec![Reply::ok(CONTENT)]);
    body_bytes(world.get(&repo, "cb/keep.bin", &alice).await).await;
    world.settle().await;
    world
        .upstream
        .route("/cb/other.bin", vec![Reply::status(500)]);
    for _ in 0..2 {
        world.get(&repo, "cb/other.bin", &alice).await;
    }
    world.clock.advance(Duration::from_secs(2 * 3600));
    let calls = world.upstream.calls();
    let response = world.get(&repo, "cb/keep.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("STALE")
    );
    assert_eq!(world.upstream.calls(), calls);
}

#[tokio::test]
async fn the_per_repository_concurrency_cap_answers_503_with_retry_after() {
    let world = World::new();
    let base = fixtures::repository(
        "capped",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Proxy,
    );
    let repo = world
        .add(reconfigure(&base, |c| c.max_concurrent_upstream = 1))
        .await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    // The first request holds the only permit for longer than permit_wait (300 ms).
    world.upstream.route(
        "/cap/slow.bin",
        vec![Reply::ok(vec![1u8; 40]).chunked(10, Duration::from_millis(150))],
    );
    world
        .upstream
        .route("/cap/fast.bin", vec![Reply::ok(CONTENT)]);
    let first = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::GET, "cap/slow.bin", &alice);
        async move {
            body_bytes(engine.serve(repo, head, Body::empty()).await)
                .await
                .len()
        }
    });
    eventually("first request holds the permit", || {
        world.upstream.calls() == 1
    })
    .await;
    let second = world.get(&repo, "cap/fast.bin", &alice).await;
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(second.headers().contains_key(header::RETRY_AFTER));
    assert_eq!(
        world.upstream.calls_for("/cap/fast.bin"),
        0,
        "no request is made without a permit"
    );
    assert_eq!(first.await.unwrap(), 40);
    world.settle().await;
    // The permit is back once the first request is done.
    assert_eq!(
        world.get(&repo, "cap/fast.bin", &alice).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_follower_gives_up_with_503_when_the_fill_takes_too_long() {
    let limits = Limits {
        singleflight_max_wait: Duration::from_millis(100),
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    world.upstream.route(
        "/sf/a.bin",
        vec![Reply::ok(CONTENT).delay_head(Duration::from_millis(220))],
    );
    let alice = principal("alice");
    let leader = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::GET, "sf/a.bin", &alice);
        async move { engine.serve(repo, head, Body::empty()).await.status() }
    });
    eventually("fill started", || world.upstream.calls() == 1).await;
    let (request, cx) = request_for(&world, &repo, "sf/a.bin");
    let error = cx
        .proxy()
        .fetch_document(ProxyFetch::new(&request))
        .await
        .unwrap_err();
    assert!(
        matches!(error, FormatError::Unavailable { .. }),
        "{error:?}"
    );
    assert!(error.retry_after().is_some());
    assert_eq!(leader.await.unwrap(), StatusCode::OK);
    assert_eq!(world.upstream.calls(), 1);
}

#[tokio::test]
async fn shutdown_aborts_a_streaming_fill_without_leaving_a_blob() {
    let (world, repo) = proxy_world().await;
    world.upstream.route(
        "/sd/a.bin",
        vec![Reply::ok(vec![1u8; 400]).chunked(10, Duration::from_millis(20))],
    );
    let response = world.get(&repo, "sd/a.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let reader = tokio::spawn(rustryx_format_core::stream::read_bounded(
        response.into_body(),
        10_000,
    ));
    tokio::time::sleep(Duration::from_millis(60)).await;
    world.shutdown.cancel();
    world.engine.drain().await;
    assert!(
        reader.await.unwrap().is_err(),
        "the client stream must break, not end cleanly"
    );
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
    assert_eq!(world.engine.fills_in_flight(), 0);
}

#[tokio::test]
async fn must_not_reach_an_upstream_that_the_policy_blocks_and_audits_it_once() {
    let world = World::new();
    let base = fixtures::repository(
        "evil",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Proxy,
    );
    let repo = world
        .add(reconfigure(&base, |c| {
            c.remote_url = "https://169.254.169.254/latest/".into()
        }))
        .await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    for i in 0..3 {
        let response = world.get(&repo, &format!("meta{i}.bin"), &alice).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(body_text(response).await, "RAW-ERROR upstream_blocked");
    }
    assert_eq!(
        world.upstream.calls(),
        0,
        "a blocked upstream must never be contacted"
    );
    let blocked: Vec<_> = world
        .audit
        .events()
        .into_iter()
        .filter(|e| e.action == "proxy.ssrf_blocked")
        .collect();
    assert_eq!(
        blocked.len(),
        1,
        "audited once per minute and repository, not once per request"
    );
    world.settle().await;
}

#[tokio::test]
async fn a_remote_url_that_is_not_a_url_is_refused() {
    let world = World::new();
    let base = fixtures::repository(
        "junk",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Proxy,
    );
    let repo = world
        .add(reconfigure(&base, |c| c.remote_url = "not a url".into()))
        .await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "a.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(world.upstream.calls(), 0);
}

#[tokio::test]
async fn cache_fill_events_are_emitted() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/ev/a.bin", vec![Reply::ok(CONTENT)]);
    body_bytes(world.get(&repo, "ev/a.bin", &principal("alice")).await).await;
    world.settle().await;
    assert!(world.events.events().iter().any(|e| matches!(
        e,
        ContentEvent::AssetPublished {
            replaced: false,
            ..
        }
    )));
}

/// An upstream whose first call panics, like a bug in a client implementation.
struct PanicsOnce {
    calls: std::sync::atomic::AtomicUsize,
    inner: Arc<rustryx_format_core::testing::FakeUpstream>,
}

#[async_trait::async_trait]
impl rustryx_format_core::proxy::upstream::UpstreamClient for PanicsOnce {
    async fn send(
        &self,
        request: rustryx_format_core::proxy::upstream::UpstreamRequest,
    ) -> Result<
        rustryx_format_core::proxy::upstream::UpstreamResponse,
        rustryx_format_core::proxy::upstream::UpstreamError,
    > {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            panic!("simulated bug in the upstream client");
        }
        self.inner.send(request).await
    }
}

#[tokio::test]
async fn a_panicking_fill_releases_the_key_and_the_request_recovers() {
    let inner = rustryx_format_core::testing::FakeUpstream::new();
    inner.route("/p/a.bin", vec![Reply::ok(CONTENT)]);
    let world = World::with_client(Arc::new(PanicsOnce {
        calls: std::sync::atomic::AtomicUsize::new(0),
        inner: inner.clone(),
    }));
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    // The first leader dies without a result. The key must not stay behind, and the
    // request becomes a leader itself instead of waiting for a ghost.
    let response = world.get(&repo, "p/a.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    world.settle().await;
    assert_eq!(world.engine.fills_in_flight(), 0);
    assert_eq!(inner.calls(), 1);
}

#[tokio::test]
async fn head_miss_maps_upstream_failures_like_a_get() {
    let (world, repo) = proxy_world().await;
    let alice = principal("alice");
    for (path, upstream, expected) in [
        ("hd/401.bin", 401u16, StatusCode::BAD_GATEWAY),
        ("hd/403.bin", 403, StatusCode::BAD_GATEWAY),
        ("hd/400.bin", 400, StatusCode::BAD_GATEWAY),
        ("hd/410.bin", 410, StatusCode::NOT_FOUND),
    ] {
        world
            .upstream
            .route(&format!("/{path}"), vec![Reply::status(upstream)]);
        let response = world
            .send(&repo, Method::HEAD, path, &alice, Body::empty())
            .await;
        assert_eq!(response.status(), expected, "{path}");
    }
    let (request, cx) = request_for(&world, &repo, "hd/s3.bin");
    world.upstream.route("/hd/s3.bin", vec![Reply::status(403)]);
    let mut head = (*request.head).clone();
    head.method = Method::HEAD;
    let head_request = ArtifactRequest::new(
        &head,
        request.path.clone(),
        rustryx_core::identity::Action::Read,
    );
    let error = cx
        .proxy()
        .fetch_artifact(ProxyFetch::new(&head_request).forbidden_is_miss())
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::NotFound));
    // A negative cache entry from a GET answers a later HEAD without a request.
    world
        .upstream
        .route("/hd/neg.bin", vec![Reply::status(404)]);
    assert_eq!(
        world.get(&repo, "hd/neg.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    let before = world.upstream.calls();
    let response = world
        .send(&repo, Method::HEAD, "hd/neg.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(world.upstream.calls(), before);
}

#[tokio::test]
async fn query_and_component_options_reach_the_upstream_and_the_store() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/opt/a.bin", vec![Reply::ok(CONTENT)]);
    let (request, cx) = request_for(&world, &repo, "opt/a.bin");
    let fetch = ProxyFetch::new(&request)
        .upstream_query("v=1&x=%20")
        .component(rustryx_format_core::ComponentSpec {
            group: Some("g".into()),
            name: "opt".into(),
            version: Some("1".into()),
        });
    let response = cx.proxy().fetch_artifact(fetch).await.unwrap();
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    world.settle().await;
    assert_eq!(
        world.upstream.requests()[0].url,
        "https://upstream.example.test/opt/a.bin?v=1&x=%20"
    );
    let filter = rustryx_core::ports::ComponentFilter {
        name: Some("opt".into()),
        ..Default::default()
    };
    let found = rustryx_core::ports::ComponentRepo::list(
        &world.store,
        &filter,
        &rustryx_core::ports::PageRequest::default(),
    )
    .await
    .unwrap();
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.items[0].group_id.as_deref(), Some("g"));
}

#[tokio::test]
async fn a_failing_component_lookup_does_not_leave_the_fetched_blob_behind() {
    struct BrokenComponents(rustryx_testkit::MemStore);
    #[async_trait::async_trait]
    impl rustryx_core::ports::ComponentRepo for BrokenComponents {
        async fn find_or_create(
            &self,
            _new: &rustryx_core::model::NewComponent,
        ) -> Result<rustryx_core::model::Component, rustryx_core::error::StoreError> {
            Err(rustryx_core::error::StoreError::Unavailable(
                "components down".into(),
            ))
        }
        async fn get(
            &self,
            id: rustryx_core::ids::ComponentId,
        ) -> Result<Option<rustryx_core::model::Component>, rustryx_core::error::StoreError>
        {
            rustryx_core::ports::ComponentRepo::get(&self.0, id).await
        }
        async fn find(
            &self,
            new: &rustryx_core::model::NewComponent,
        ) -> Result<Option<rustryx_core::model::Component>, rustryx_core::error::StoreError>
        {
            self.0.find(new).await
        }
        async fn list(
            &self,
            filter: &rustryx_core::ports::ComponentFilter,
            page: &rustryx_core::ports::PageRequest,
        ) -> Result<
            rustryx_core::ports::Page<rustryx_core::model::Component>,
            rustryx_core::error::StoreError,
        > {
            rustryx_core::ports::ComponentRepo::list(&self.0, filter, page).await
        }
        async fn delete(
            &self,
            id: rustryx_core::ids::ComponentId,
        ) -> Result<(), rustryx_core::error::StoreError> {
            rustryx_core::ports::ComponentRepo::delete(&self.0, id).await
        }
    }
    let world = World::with_ports(|ports| {
        ports.components = Arc::new(BrokenComponents(rustryx_testkit::MemStore::new()));
    });
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    world.upstream.route("/c/a.index", vec![Reply::ok(CONTENT)]);
    let (request, cx) = request_for(&world, &repo, "c/a.index");
    let fetch = ProxyFetch::new(&request).component(rustryx_format_core::ComponentSpec {
        group: None,
        name: "c".into(),
        version: None,
    });
    let error = cx.proxy().fetch_document(fetch).await.unwrap_err();
    assert!(matches!(error, FormatError::Internal(_)), "{error:?}");
    assert_eq!(
        world.blobs.len(),
        0,
        "a fetched but unpublished blob must be deleted"
    );
    assert!(
        world
            .store
            .get_by_path(repo.id, "c/a.index")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_failing_publish_after_a_streamed_fill_leaves_no_blob() {
    let (world, flaky) = World::with_flaky_store();
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    world.upstream.route("/f/a.bin", vec![Reply::ok(CONTENT)]);
    let store = world.store.clone();
    flaky.after_commit(move || store.set_unavailable(true));
    // The client is served from the tee, so it still gets its bytes.
    let response = world.get(&repo, "f/a.bin", &principal("alice")).await;
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    world.settle().await;
    world.store.set_unavailable(false);
    assert_eq!(
        flaky.inner.len(),
        0,
        "the committed blob of a failed publish is removed"
    );
}

#[tokio::test]
async fn a_blob_store_that_breaks_mid_stream_degrades_to_pass_through_for_the_client() {
    let (world, repo) = proxy_world().await;
    world.upstream.route(
        "/mid/a.bin",
        vec![Reply::ok(vec![2u8; 200]).chunked(20, Duration::from_millis(30))],
    );
    let response = world.get(&repo, "mid/a.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let reader = tokio::spawn(body_bytes(response));
    eventually("fill is writing", || world.blobs.staging_count() == 1).await;
    world.blobs.set_unavailable(true);
    let body = reader.await.unwrap();
    assert_eq!(
        body.len(),
        200,
        "the client keeps its download although the cache broke"
    );
    world.settle().await;
    world.blobs.set_unavailable(false);
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
}

#[tokio::test]
async fn verified_fetches_fail_cleanly_when_the_blob_store_is_unavailable() {
    let (world, repo) = proxy_world().await;
    world.upstream.route("/v/a.bin", vec![Reply::ok(CONTENT)]);
    let sha = fixtures::checksums_of(CONTENT).sha256;
    world.blobs.set_unavailable(true);
    let (request, cx) = request_for(&world, &repo, "v/a.bin");
    let error = cx
        .proxy()
        .fetch_artifact(
            ProxyFetch::new(&request)
                .expect(ExpectedChecksum::new(HashAlgo::Sha256, &sha).unwrap()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    world.blobs.set_unavailable(false);

    // A verified body over the size limit cannot be cached and is not served.
    let limits = Limits {
        max_blob_bytes: 10,
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    world.upstream.route("/v/big.bin", vec![Reply::ok(CONTENT)]);
    let (request, cx) = request_for(&world, &repo, "v/big.bin");
    let error = cx
        .proxy()
        .fetch_artifact(
            ProxyFetch::new(&request)
                .expect(ExpectedChecksum::new(HashAlgo::Sha256, &sha).unwrap()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn a_streamed_body_over_the_limit_without_a_declared_length_is_passed_through_uncached() {
    let limits = Limits {
        max_blob_bytes: 30,
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    // No Content-Length (chunked upstream), so the limit is only hit while streaming.
    world.upstream.route(
        "/nolen.bin",
        vec![
            Reply::ok(vec![8u8; 100])
                .without_content_length()
                .chunked(10, Duration::ZERO),
        ],
    );
    let response = world.get(&repo, "nolen.bin", &principal("alice")).await;
    assert!(response.headers().get("content-length").is_none());
    assert_eq!(body_bytes(response).await.len(), 100);
    world.settle().await;
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
}
