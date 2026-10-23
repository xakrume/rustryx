//! Proxy cache revalidation (`touch_fresh`, conditional requests), create-only fills and what
//! happens when the transaction refuses to store a fill.
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

use axum_core::body::Body;
use http::{Method, StatusCode};
use rustryx_core::error::PolicyViolation;
use rustryx_core::ids::ImmutabilityRuleId;
use rustryx_core::model::{PublishMode, Repository};
use rustryx_core::ports::{AssetRepo, Clock};
use rustryx_core::quota::{QuotaExceeded, QuotaScope};
use rustryx_format_core::testing::Reply;
use support::doubles::{AssetSpy, Fault};
use support::recorder::TestRecorder;
use support::{World, body_bytes, body_text, header_of, principal};

const CONTENT: &[u8] = b"proxied artifact bytes";
const DATE: &str = "Wed, 21 Oct 2026 07:28:00 GMT";

async fn spied_world() -> (World, Arc<AssetSpy>, Arc<Repository>) {
    let spy = AssetSpy::new();
    let world = World::with_ports(|ports| spy.install(ports));
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    (world, spy, repo)
}

fn versioned(body: &'static [u8], etag: &str) -> Reply {
    Reply::ok(body)
        .header("etag", etag)
        .header("last-modified", DATE)
}

async fn fill_and_expire(world: &World, repo: &Arc<Repository>, path: &str) {
    body_bytes(world.get(repo, path, &principal("alice")).await).await;
    world.settle().await;
    world.clock.advance(Duration::from_secs(2 * 3600));
}

#[tokio::test]
async fn an_expired_copy_is_revalidated_with_a_conditional_request_and_not_fetched_again() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let (world, spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/r/a.bin",
        vec![versioned(CONTENT, "\"v1\""), Reply::status(304)],
    );
    fill_and_expire(&world, &repo, "r/a.bin").await;

    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT"),
        "a confirmed copy is served as a hit"
    );
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    world.settle().await;

    let requests = world.upstream.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].headers.get("if-none-match").is_none());
    assert_eq!(requests[1].headers.get("if-none-match").unwrap(), "\"v1\"");
    assert_eq!(requests[1].headers.get("if-modified-since").unwrap(), DATE);

    assert_eq!(spy.touches(), 1, "the age is reset with touch_fresh");
    assert_eq!(world.blobs.len(), 1, "no second copy of the bytes");
    assert_eq!(world.events.orphaned(), 0);
    let asset = world
        .store
        .get_by_path(repo.id, "r/a.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(asset.updated_at, world.clock.now());

    // The copy is fresh again, so the next read does not ask the upstream at all.
    world.clock.advance(Duration::from_secs(60));
    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT")
    );
    assert_eq!(world.upstream.calls(), 2);
    assert_eq!(
        recorder.counter(
            "proxy_cache_total",
            &[("format", "raw"), ("result", "revalidated")]
        ),
        1
    );
}

#[tokio::test]
async fn must_not_send_the_validators_of_a_replaced_copy() {
    let (world, _spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/r/a.bin",
        vec![
            versioned(b"version one", "\"v1\""),
            versioned(b"version two!", "\"v2\""),
            Reply::status(304),
        ],
    );
    fill_and_expire(&world, &repo, "r/a.bin").await;
    // The first expiry asks with v1 and the upstream has a newer version.
    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(&body_bytes(response).await[..], b"version two!");
    world.settle().await;
    world.clock.advance(Duration::from_secs(2 * 3600));
    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(&body_bytes(response).await[..], b"version two!");

    let requests = world.upstream.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].headers.get("if-none-match").unwrap(), "\"v1\"");
    assert_eq!(
        requests[2].headers.get("if-none-match").unwrap(),
        "\"v2\"",
        "the validator of the replaced bytes must never be sent for the new bytes"
    );
}

#[tokio::test]
async fn an_unchanged_body_without_a_304_does_not_replace_the_blob() {
    let (world, spy, repo) = spied_world().await;
    let alice = principal("alice");
    // No validators at all: the upstream cannot answer 304, it sends the same body again.
    world
        .upstream
        .route("/r/a.bin", vec![Reply::ok(CONTENT), Reply::ok(CONTENT)]);
    fill_and_expire(&world, &repo, "r/a.bin").await;

    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    world.settle().await;

    assert!(
        world.upstream.requests()[1]
            .headers
            .get("if-none-match")
            .is_none()
    );
    assert_eq!(world.blobs.len(), 1, "the identical body must be dropped");
    assert_eq!(world.blobs.staging_count(), 0);
    assert_eq!(world.events.orphaned(), 0, "nothing was replaced");
    assert_eq!(spy.touches(), 1);
    assert_eq!(
        spy.publishes(),
        1,
        "only the first fill published, the refresh did not"
    );
    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT")
    );
    assert_eq!(world.upstream.calls(), 2);
}

#[tokio::test]
async fn an_unchanged_index_document_is_served_as_a_hit_after_the_refetch() {
    let (world, spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/pkg/list.index",
        vec![Reply::ok(&b"one\ntwo"[..]), Reply::ok(&b"one\ntwo"[..])],
    );
    body_bytes(world.get(&repo, "pkg/list.index", &alice).await).await;
    world.clock.advance(Duration::from_secs(700));
    let response = world.get(&repo, "pkg/list.index", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT"),
        "the refetched document equals the copy, which is therefore current"
    );
    assert_eq!(&body_bytes(response).await[..], b"one\ntwo");
    assert_eq!(world.blobs.len(), 1);
    assert_eq!(spy.touches(), 1);
}

#[tokio::test]
async fn validators_are_remembered_after_an_unchanged_full_fetch() {
    let (world, _spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/r/a.bin",
        vec![
            Reply::ok(CONTENT),
            versioned(CONTENT, "\"v9\""),
            Reply::status(304),
        ],
    );
    fill_and_expire(&world, &repo, "r/a.bin").await;
    body_bytes(world.get(&repo, "r/a.bin", &alice).await).await;
    world.settle().await;
    world.clock.advance(Duration::from_secs(2 * 3600));
    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    let requests = world.upstream.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].headers.get("if-none-match").unwrap(),
        "\"v9\"",
        "the validators of the answer that confirmed the copy are the ones to ask with"
    );
}

#[tokio::test]
async fn a_node_without_validators_asks_unconditionally() {
    let (world, _spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/r/a.bin",
        vec![versioned(CONTENT, "\"v1\""), Reply::ok(CONTENT)],
    );
    fill_and_expire(&world, &repo, "r/a.bin").await;

    // Another node never saw the first answer, so it has nothing to ask with.
    let other = world.second_node();
    let head = world.head(Method::GET, "r/a.bin", &alice);
    let response = other.serve(repo.clone(), head, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    let requests = world.upstream.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].headers.get("if-none-match").is_none(),
        "{:?}",
        requests[1].headers
    );
    other.drain().await;
    // An identical body does not replace the blob on this path either.
    assert_eq!(world.blobs.len(), 1);
}

#[tokio::test]
async fn must_not_forward_an_unasked_304_as_content() {
    let (world, spy, repo) = spied_world().await;
    world.upstream.route("/u/a.bin", vec![Reply::status(304)]);
    let response = world.get(&repo, "u/a.bin", &principal("alice")).await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "a 304 for a request that carried no condition is a broken upstream"
    );
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(spy.touches(), 0);
}

#[tokio::test]
async fn a_touch_that_finds_no_asset_is_not_an_error() {
    let (world, spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/r/a.bin",
        vec![versioned(CONTENT, "\"v1\""), Reply::status(304)],
    );
    fill_and_expire(&world, &repo, "r/a.bin").await;
    *spy.touch_fresh_fault.lock().unwrap() = Some(Fault::NotFound);
    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    assert_eq!(spy.touches(), 1);
}

#[tokio::test]
async fn a_failing_touch_serves_the_expired_copy_as_stale() {
    let (world, spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/r/a.bin",
        vec![versioned(CONTENT, "\"v1\""), Reply::status(304)],
    );
    fill_and_expire(&world, &repo, "r/a.bin").await;
    *spy.touch_fresh_fault.lock().unwrap() = Some(Fault::Unavailable);
    let response = world.get(&repo, "r/a.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("STALE")
    );
    assert_eq!(&body_bytes(response).await[..], CONTENT);
}

#[tokio::test]
async fn fills_publish_create_only_unless_they_refresh_an_expired_copy() {
    let (world, spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/r/a.bin",
        vec![
            Reply::ok(&b"version one"[..]),
            Reply::ok(&b"version two!"[..]),
        ],
    );
    let mut head = world.head(Method::GET, "r/a.bin", &alice);
    head.client.request_id = Some("req-9".into());
    body_bytes(world.engine.serve(repo.clone(), head, Body::empty()).await).await;
    world.settle().await;
    world.clock.advance(Duration::from_secs(2 * 3600));
    body_bytes(world.get(&repo, "r/a.bin", &alice).await).await;
    world.settle().await;

    let options = spy.publish_options.lock().unwrap().clone();
    assert_eq!(options.len(), 2);
    assert_eq!(
        options[0].mode,
        PublishMode::CreateOnly,
        "a fill without a copy must never replace one published meanwhile, on any node"
    );
    assert_eq!(options[1].mode, PublishMode::Upsert, "a refresh replaces");
    assert!(options[0].actor.is_none(), "a fill is the system's doing");
    assert_eq!(options[0].request_id.as_deref(), Some("req-9"));
}

#[tokio::test]
async fn a_fill_that_lost_the_create_race_discards_its_copy_and_serves_the_winner() {
    let (world, _spy, repo) = spied_world().await;
    let alice = principal("alice");
    // The winner's copy, as the other node's fill would have published it.
    let winner_key = rustryx_core::ports::BlobKey::new("44444444-4444-7444-8444-444444444444");
    world
        .blobs
        .put(&winner_key, bytes::Bytes::from_static(b"the winner"));
    let mut record = rustryx_testkit::fixtures::blob_record(winner_key.as_str(), b"the winner");
    record.blob_store_id = repo.blob_store_id;
    world.upstream.route(
        "/r/a.index",
        vec![Reply::ok(&b"from upstream"[..]).delay_head(Duration::from_millis(80))],
    );
    let pending = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::GET, "r/a.index", &alice);
        async move { engine.serve(repo, head, Body::empty()).await }
    });
    support::eventually("fill started", || world.upstream.calls() == 1).await;
    world
        .store
        .publish_with(
            &rustryx_core::model::NewAsset::new(repo.id, "r/a.index", "text/plain", record),
            &rustryx_core::model::WriteOptions::new().with_mode(PublishMode::CreateOnly),
        )
        .await
        .unwrap();
    let response = pending.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        &body_bytes(response).await[..],
        b"the winner",
        "the copy that exists is served, not the one this fill fetched"
    );
    world.settle().await;
    assert_eq!(
        world.blobs.len(),
        1,
        "the surplus copy of the fill is deleted"
    );
}

fn violations() -> Vec<(PolicyViolation, u16, &'static str)> {
    vec![
        (
            PolicyViolation::Immutable {
                rule: ImmutabilityRuleId::generate(),
            },
            409,
            "RAW-ERROR immutable",
        ),
        (
            PolicyViolation::QuotaExceeded(QuotaExceeded {
                scope: QuotaScope::Repository(rustryx_core::ids::RepositoryId::generate()),
                limit_bytes: 1,
                used_bytes: 1,
                requested_bytes: 1,
            }),
            507,
            "RAW-ERROR quota_exceeded",
        ),
        (PolicyViolation::ReadOnly, 503, "RAW-ERROR read_only_mode"),
    ]
}

#[tokio::test]
async fn must_not_store_a_fill_that_the_transaction_refuses() {
    for (violation, status, text) in violations() {
        let recorder = TestRecorder::new();
        let _guard = recorder.install();
        let (world, spy, repo) = spied_world().await;
        *spy.refuse_publish.lock().unwrap() = Some(violation.clone());
        // An index document is verified before it is served: nothing is streamed to the client
        // ahead of the metadata commit, so the refusal is what the client sees.
        world
            .upstream
            .route("/pkg/list.index", vec![Reply::ok(&b"one"[..])]);
        let response = world
            .get(&repo, "pkg/list.index", &principal("alice"))
            .await;
        assert_eq!(response.status().as_u16(), status, "{violation:?}");
        assert_eq!(body_text(response).await, text);
        world.settle().await;
        assert_eq!(world.blobs.len(), 0, "{violation:?}: the object is deleted");
        assert_eq!(
            recorder.counter(
                "policy_violations_total",
                &[("format", "raw"), ("reason", violation.label())]
            ),
            1,
            "{violation:?}"
        );
    }
}

#[tokio::test]
async fn an_expired_copy_that_policy_refuses_to_replace_is_served_as_stale() {
    let (world, spy, repo) = spied_world().await;
    let alice = principal("alice");
    world.upstream.route(
        "/pkg/list.index",
        vec![Reply::ok(&b"one"[..]), Reply::ok(&b"two, newer"[..])],
    );
    body_bytes(world.get(&repo, "pkg/list.index", &alice).await).await;
    // The copy became immutable (a rule was added) after it was cached.
    *spy.refuse_publish.lock().unwrap() = Some(PolicyViolation::Immutable {
        rule: ImmutabilityRuleId::generate(),
    });
    world.clock.advance(Duration::from_secs(700));
    let response = world.get(&repo, "pkg/list.index", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("STALE")
    );
    assert_eq!(&body_bytes(response).await[..], b"one");
    world.settle().await;
    assert_eq!(world.blobs.len(), 1, "the refused replacement is deleted");
}
