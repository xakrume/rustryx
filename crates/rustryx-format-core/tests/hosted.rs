//! Hosted pipeline through the engine and `RawLikeHandler`.
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

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderValue, Method, StatusCode, header};
use rustryx_core::audit::AuditKind;
use rustryx_core::model::WritePolicy;
use rustryx_core::ports::{
    AssetRepo, BlobKey, BlobRepo, BlobStore, BlobStoreRepo, ComponentFilter, ComponentRepo,
    PageRequest,
};
use rustryx_format_core::{
    ContentEvent, FormatError, Limits, NormalizedPath, OverwriteRule, UploadSpec,
};
use rustryx_testkit::fixtures;
use support::{
    World, anonymous, body_bytes, body_text, channel_body, eventually, header_of, principal,
    tracked_body,
};

const CONTENT: &[u8] = b"0123456789abcdef";

async fn world_with_hosted() -> (World, std::sync::Arc<rustryx_core::model::Repository>) {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    (world, repo)
}

#[tokio::test]
async fn upload_then_download_round_trips_with_full_headers() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");

    let response = world.put(&repo, "pkg/data.bin", &alice, CONTENT).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(world.blobs.len(), 1);
    assert_eq!(world.blobs.staging_count(), 0);

    let response = world.get(&repo, "pkg/data.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    let sums = fixtures::checksums_of(CONTENT);
    let headers = response.headers().clone();
    assert_eq!(header_of(&headers, "content-length"), Some("16"));
    assert_eq!(
        header_of(&headers, "etag"),
        Some(format!("\"{}\"", sums.sha256).as_str())
    );
    assert_eq!(
        header_of(&headers, "x-checksum-sha1"),
        Some(sums.sha1.as_str())
    );
    assert_eq!(
        header_of(&headers, "x-checksum-sha256"),
        Some(sums.sha256.as_str())
    );
    assert_eq!(
        header_of(&headers, "x-checksum-md5"),
        Some(sums.md5.as_str())
    );
    assert_eq!(header_of(&headers, "accept-ranges"), Some("bytes"));
    assert_eq!(
        header_of(&headers, "x-content-type-options"),
        Some("nosniff")
    );
    assert!(
        header_of(&headers, "content-security-policy")
            .unwrap()
            .contains("sandbox")
    );
    assert!(
        header_of(&headers, "last-modified")
            .unwrap()
            .ends_with("GMT")
    );
    assert_eq!(
        header_of(&headers, "content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(&body_bytes(response).await[..], CONTENT);

    // The upload created the component and one audit record.
    let filter = ComponentFilter {
        name: Some("pkg".into()),
        ..ComponentFilter::default()
    };
    let components = ComponentRepo::list(&world.store, &filter, &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(components.items.len(), 1);
    let events = world.audit.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, AuditKind::ContentChange);
    assert_eq!(events[0].action, "asset.publish");
    assert_eq!(events[0].actor_name, "alice");
    assert_eq!(events[0].outcome, "success");
}

#[tokio::test]
async fn content_type_is_guessed_from_the_extension() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    world.put(&repo, "lib/x.jar", &alice, CONTENT).await;
    let response = world.get(&repo, "lib/x.jar", &alice).await;
    assert_eq!(
        header_of(response.headers(), "content-type"),
        Some("application/java-archive")
    );
}

#[tokio::test]
async fn must_not_store_a_declared_html_content_type() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    let mut head = world.head(Method::PUT, "page/x.bin", &alice);
    head.headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
    let response = world.send_with(&repo, head, Body::from(CONTENT)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = world.get(&repo, "page/x.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "content-type"),
        Some("application/octet-stream")
    );
}

#[tokio::test]
async fn overwrite_replaces_the_asset_and_announces_the_replaced_blob() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    world.put(&repo, "pkg/a.bin", &alice, b"first").await;
    let response = world.put(&repo, "pkg/a.bin", &alice, b"second!").await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a replaced path answers 200"
    );
    assert_eq!(
        body_bytes(world.get(&repo, "pkg/a.bin", &alice).await).await,
        Bytes::from_static(b"second!")
    );
    assert_eq!(world.events.orphaned(), 1);
    assert!(
        world
            .events
            .events()
            .iter()
            .any(|e| matches!(e, ContentEvent::AssetPublished { replaced: true, .. }))
    );
    // The old blob is only unreferenced, GC removes the object.
    assert_eq!(world.blobs.len(), 2);
    let actions: Vec<_> = world.audit.events().into_iter().map(|e| e.action).collect();
    assert_eq!(actions, ["asset.publish", "asset.overwrite"]);
}

#[tokio::test]
async fn allow_once_answers_conflict_and_keeps_the_original() {
    let world = World::new();
    let mut repository = fixtures::repository(
        "once",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repository.write_policy = WritePolicy::AllowOnce;
    let repo = world.add(repository).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");

    assert_eq!(
        world
            .put(&repo, "a/one.bin", &alice, b"original")
            .await
            .status(),
        StatusCode::CREATED
    );
    let second = world.put(&repo, "a/one.bin", &alice, b"changed!").await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert!(body_text(second).await.contains("conflict"));
    assert_eq!(
        body_bytes(world.get(&repo, "a/one.bin", &alice).await).await,
        Bytes::from_static(b"original")
    );
    assert_eq!(
        world.blobs.len(),
        1,
        "the rejected upload must not leave a blob"
    );
    assert_eq!(world.blobs.staging_count(), 0);
}

#[tokio::test]
async fn must_not_allow_two_concurrent_uploads_of_an_immutable_path() {
    let world = World::new();
    let mut repository = fixtures::repository(
        "once",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repository.write_policy = WritePolicy::AllowOnce;
    let repo = world.add(repository).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");

    let (tx, body) = channel_body();
    let first = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::PUT, "race/x.bin", &alice);
        async move { engine.serve(repo, head, body).await.status() }
    });
    eventually("first upload staged", || world.blobs.staging_count() == 1).await;
    tx.send(Ok(Bytes::from_static(b"abc"))).await.unwrap();

    let second = world.put(&repo, "race/x.bin", &alice, b"other").await;
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "the path is reserved by the first upload"
    );

    drop(tx);
    assert_eq!(first.await.unwrap(), StatusCode::CREATED);
    assert_eq!(
        body_bytes(world.get(&repo, "race/x.bin", &alice).await).await,
        Bytes::from_static(b"abc")
    );
    // After the first upload finished the reservation is gone: a third upload now
    // conflicts because the path exists, not because it is reserved.
    let third = world.put(&repo, "race/x.bin", &alice, b"late").await;
    assert_eq!(third.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn read_only_repository_refuses_writes_and_deletes() {
    let world = World::new();
    let mut repository = fixtures::repository(
        "ro",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repository.write_policy = WritePolicy::Deny;
    let repo = world.add(repository).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    assert_eq!(
        world.put(&repo, "a/b.bin", &alice, CONTENT).await.status(),
        StatusCode::FORBIDDEN
    );
    let delete = world
        .send(&repo, Method::DELETE, "a/b.bin", &alice, Body::empty())
        .await;
    assert_eq!(delete.status(), StatusCode::FORBIDDEN);
    assert_eq!(world.blobs.len(), 0);
}

#[tokio::test]
async fn delete_removes_the_asset_and_announces_the_blob() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    world.put(&repo, "pkg/gone.bin", &alice, CONTENT).await;
    let response = world
        .send(&repo, Method::DELETE, "pkg/gone.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        world.get(&repo, "pkg/gone.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(world.events.orphaned(), 1);
    assert_eq!(world.audit.events().last().unwrap().action, "asset.delete");
    let again = world
        .send(&repo, Method::DELETE, "pkg/gone.bin", &alice, Body::empty())
        .await;
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn must_not_leave_a_partial_blob_when_the_body_breaks() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    let (tx, body) = channel_body();
    tx.send(Ok(Bytes::from_static(b"partial"))).await.unwrap();
    tx.send(Err(std::io::Error::other("connection reset")))
        .await
        .unwrap();
    let response = world
        .send(&repo, Method::PUT, "pkg/broken.bin", &alice, body)
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(
        world.blobs.staging_count(),
        0,
        "the staged blob must be aborted"
    );
    assert!(
        world
            .store
            .get_by_path(repo.id, "pkg/broken.bin")
            .await
            .unwrap()
            .is_none()
    );
    assert!(world.audit.events().is_empty());
}

#[tokio::test]
async fn must_not_leave_a_partial_blob_when_the_client_goes_away() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    let (tx, body) = channel_body();
    let request = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::PUT, "pkg/abandoned.bin", &alice);
        async move { engine.serve(repo, head, body).await }
    });
    tx.send(Ok(Bytes::from_static(b"some bytes")))
        .await
        .unwrap();
    eventually("upload staged", || world.blobs.staging_count() == 1).await;
    // The connection closes: the framework drops the handler future.
    request.abort();
    let _ = request.await;
    eventually("staging cleaned", || world.blobs.staging_count() == 0).await;
    assert_eq!(world.blobs.len(), 0);
    assert!(
        world
            .store
            .get_by_path(repo.id, "pkg/abandoned.bin")
            .await
            .unwrap()
            .is_none()
    );
    drop(tx);
    // A reservation or lock must not outlive the cancelled request.
    assert_eq!(
        world
            .put(&repo, "pkg/abandoned.bin", &alice, b"retry")
            .await
            .status(),
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn a_stalled_client_body_times_out_with_408() {
    let limits = Limits {
        body_idle_timeout: Duration::from_millis(80),
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let (tx, body) = channel_body();
    tx.send(Ok(Bytes::from_static(b"a"))).await.unwrap();
    let response = world
        .send(
            &repo,
            Method::PUT,
            "pkg/slow.bin",
            &principal("alice"),
            body,
        )
        .await;
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(world.blobs.staging_count(), 0);
    assert_eq!(world.blobs.len(), 0);
    drop(tx);
}

#[tokio::test]
async fn declared_length_must_match_the_received_bytes() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    for declared in ["3", "100"] {
        let mut head = world.head(Method::PUT, "pkg/len.bin", &alice);
        head.headers
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static(declared));
        let response = world.send_with(&repo, head, Body::from(CONTENT)).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "declared {declared}"
        );
    }
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
}

#[tokio::test]
async fn must_not_read_the_body_when_the_declared_size_exceeds_the_limit() {
    let limits = Limits {
        max_blob_bytes: 10,
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    let (polled, body) = tracked_body(CONTENT);
    let mut head = world.head(Method::PUT, "pkg/big.bin", &alice);
    head.headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from_static("16"));
    let response = world.send_with(&repo, head, body).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        !polled.load(Ordering::SeqCst),
        "an oversized upload is refused before its body is read"
    );
    assert_eq!(world.blobs.staging_count(), 0);
}

#[tokio::test]
async fn must_not_accept_more_bytes_than_the_limit_when_length_is_not_declared() {
    let limits = Limits {
        max_blob_bytes: 10,
        ..Limits::default()
    };
    let world = World::with(limits);
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let response = world
        .put(&repo, "pkg/big.bin", &principal("alice"), CONTENT)
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
}

#[tokio::test]
async fn repository_quota_is_enforced_by_declared_and_by_actual_size() {
    let world = World::new();
    let mut repository = fixtures::repository(
        "small",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repository.quota_bytes = Some(10);
    let repo = world.add(repository).await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");

    // Declared too large: refused up front.
    let mut head = world.head(Method::PUT, "q/a.bin", &alice);
    head.headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from_static("11"));
    assert_eq!(
        world
            .send_with(&repo, head, Body::from(&b"01234567890"[..]))
            .await
            .status(),
        StatusCode::INSUFFICIENT_STORAGE
    );
    // Not declared: cut at the limit while streaming.
    assert_eq!(
        world
            .put(&repo, "q/a.bin", &alice, b"0123456789a")
            .await
            .status(),
        StatusCode::INSUFFICIENT_STORAGE
    );
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);

    // 6 bytes fit, a second 6 byte path does not, replacing the first with 9 bytes does.
    assert_eq!(
        world
            .put(&repo, "q/a.bin", &alice, b"012345")
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        world
            .put(&repo, "q/b.bin", &alice, b"012345")
            .await
            .status(),
        StatusCode::INSUFFICIENT_STORAGE
    );
    assert_eq!(
        world
            .put(&repo, "q/a.bin", &alice, b"012345678")
            .await
            .status(),
        StatusCode::OK
    );
    // Reads keep working over quota.
    assert_eq!(
        world.get(&repo, "q/a.bin", &alice).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn blob_store_quota_is_enforced() {
    let world = World::new();
    let mut definition = fixtures::blob_store_def();
    definition.quota_bytes = Some(10);
    BlobStoreRepo::delete(&world.store, definition.id)
        .await
        .unwrap();
    BlobStoreRepo::create(&world.store, &definition)
        .await
        .unwrap();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    assert_eq!(
        world
            .put(&repo, "s/a.bin", &alice, b"0123456")
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        world
            .put(&repo, "s/b.bin", &alice, b"0123456")
            .await
            .status(),
        StatusCode::INSUFFICIENT_STORAGE
    );
}

#[tokio::test]
async fn a_checksum_mismatch_deletes_the_committed_blob_and_audits() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    let head = world.head(Method::PUT, "pkg/x.bin", &alice);
    let cx = world.engine.context(repo.clone(), &head);
    let mut spec = UploadSpec::new(NormalizedPath::parse("pkg/x.bin").unwrap());
    spec.expected = Some(
        rustryx_format_core::ExpectedChecksum::new(
            rustryx_format_core::HashAlgo::Sha256,
            &"0".repeat(64),
        )
        .unwrap(),
    );
    let error = cx
        .hosted()
        .upload(spec, Body::from(CONTENT))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::DigestMismatch), "{error:?}");
    assert_eq!(
        world.blobs.len(),
        0,
        "a rejected upload must not leave its object behind"
    );
    assert!(
        world
            .store
            .get_by_path(repo.id, "pkg/x.bin")
            .await
            .unwrap()
            .is_none()
    );
    let events = world.audit.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "asset.rejected");
    assert_eq!(events[0].outcome, "failure");
}

#[tokio::test]
async fn a_matching_declared_digest_publishes() {
    let (world, repo) = world_with_hosted().await;
    let head = world.head(Method::PUT, "pkg/ok.bin", &principal("alice"));
    let cx = world.engine.context(repo.clone(), &head);
    let mut spec = UploadSpec::new(NormalizedPath::parse("pkg/ok.bin").unwrap());
    spec.expected = Some(
        rustryx_format_core::ExpectedChecksum::new(
            rustryx_format_core::HashAlgo::Sha256,
            &fixtures::checksums_of(CONTENT).sha256,
        )
        .unwrap(),
    );
    let outcome = cx.hosted().upload(spec, Body::from(CONTENT)).await.unwrap();
    assert!(outcome.created());
    assert_eq!(outcome.asset.size_bytes, 16);
}

#[tokio::test]
async fn immutable_rule_overrides_an_allow_policy() {
    let (world, repo) = world_with_hosted().await;
    let head = world.head(Method::PUT, "pkg/v1.bin", &principal("alice"));
    let cx = world.engine.context(repo.clone(), &head);
    let spec = || {
        let mut spec = UploadSpec::new(NormalizedPath::parse("pkg/v1.bin").unwrap());
        spec.overwrite = OverwriteRule::Never;
        spec
    };
    cx.hosted()
        .upload(spec(), Body::from(CONTENT))
        .await
        .unwrap();
    let error = cx
        .hosted()
        .upload(spec(), Body::from(&b"again"[..]))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::Conflict(_)));
    assert_eq!(world.blobs.len(), 1);
}

#[tokio::test]
async fn a_failed_publish_deletes_the_committed_blob() {
    let (world, flaky) = World::with_flaky_store();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let store = world.store.clone();
    // Right after the blob is committed the metadata store goes down, so the
    // component lookup before the publish fails.
    flaky.after_commit(move || store.set_unavailable(true));
    let response = world
        .put(&repo, "pkg/x.bin", &principal("alice"), CONTENT)
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    world.store.set_unavailable(false);
    assert_eq!(
        flaky.inner.len(),
        0,
        "a committed but unpublished blob must be deleted"
    );
    assert!(
        world
            .store
            .get_by_path(repo.id, "pkg/x.bin")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_failing_commit_leaves_nothing_and_answers_503() {
    let (world, flaky) = World::with_flaky_store();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    flaky.fail_commit(true);
    let response = world
        .put(&repo, "pkg/x.bin", &principal("alice"), CONTENT)
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(flaky.inner.len(), 0);
    assert_eq!(flaky.inner.staging_count(), 0);
    world.settle().await;
}

#[tokio::test]
async fn metadata_store_outage_answers_503_with_retry_after() {
    let (world, repo) = world_with_hosted().await;
    world.store.set_unavailable(true);
    let response = world.get(&repo, "pkg/x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    let text = body_text(response).await;
    assert!(text.contains("unavailable"));
    world.store.set_unavailable(false);
}

#[tokio::test]
async fn missing_blob_store_answers_503() {
    let world = World::without_blob_store();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let response = world
        .put(&repo, "pkg/x.bin", &principal("alice"), CONTENT)
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn range_requests() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    world.put(&repo, "pkg/r.bin", &alice, CONTENT).await;
    let ranged = |value: &'static str| {
        let mut head = world.head(Method::GET, "pkg/r.bin", &alice);
        head.headers
            .insert(header::RANGE, HeaderValue::from_static(value));
        head
    };

    let response = world
        .send_with(&repo, ranged("bytes=2-4"), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header_of(response.headers(), "content-range"),
        Some("bytes 2-4/16")
    );
    assert_eq!(header_of(response.headers(), "content-length"), Some("3"));
    assert_eq!(&body_bytes(response).await[..], b"234");

    let response = world
        .send_with(&repo, ranged("bytes=-4"), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body_bytes(response).await[..], b"cdef");

    let response = world
        .send_with(&repo, ranged("bytes=10-"), Body::empty())
        .await;
    assert_eq!(
        header_of(response.headers(), "content-range"),
        Some("bytes 10-15/16")
    );

    let response = world
        .send_with(&repo, ranged("bytes=99-"), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        header_of(response.headers(), "content-range"),
        Some("bytes */16")
    );

    let response = world
        .send_with(&repo, ranged("bytes=0-1,4-5"), Body::empty())
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "multiple ranges are ignored"
    );
    assert_eq!(body_bytes(response).await.len(), 16);
}

#[tokio::test]
async fn conditional_requests_and_head() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    world.put(&repo, "pkg/c.bin", &alice, CONTENT).await;
    let etag = format!("\"{}\"", fixtures::checksums_of(CONTENT).sha256);

    let mut head = world.head(Method::GET, "pkg/c.bin", &alice);
    head.headers
        .insert(header::IF_NONE_MATCH, HeaderValue::from_str(&etag).unwrap());
    let response = world.send_with(&repo, head, Body::empty()).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(header_of(response.headers(), "etag"), Some(etag.as_str()));
    assert!(body_bytes(response).await.is_empty());

    let mut head = world.head(Method::GET, "pkg/c.bin", &alice);
    head.headers
        .insert(header::IF_MATCH, HeaderValue::from_static("\"other\""));
    assert_eq!(
        world.send_with(&repo, head, Body::empty()).await.status(),
        StatusCode::PRECONDITION_FAILED
    );

    let mut head = world.head(Method::GET, "pkg/c.bin", &alice);
    head.headers
        .insert(header::RANGE, HeaderValue::from_static("bytes=0-3"));
    head.headers
        .insert(header::IF_RANGE, HeaderValue::from_static("\"stale\""));
    let response = world.send_with(&repo, head, Body::empty()).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a stale If-Range yields the whole body"
    );

    let response = world
        .send(&repo, Method::HEAD, "pkg/c.bin", &alice, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_of(response.headers(), "content-length"), Some("16"));
    assert!(body_bytes(response).await.is_empty());
}

#[tokio::test]
async fn missing_asset_is_404_and_missing_blob_object_is_a_server_error() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    assert_eq!(
        world.get(&repo, "nothing/here.bin", &alice).await.status(),
        StatusCode::NOT_FOUND
    );
    world.put(&repo, "pkg/lost.bin", &alice, CONTENT).await;
    let asset = world
        .store
        .get_by_path(repo.id, "pkg/lost.bin")
        .await
        .unwrap()
        .unwrap();
    let record = BlobRepo::get(&world.store, asset.blob_id)
        .await
        .unwrap()
        .unwrap();
    world.blobs.delete(&BlobKey::new(record.key)).await.unwrap();
    let response = world.get(&repo, "pkg/lost.bin", &alice).await;
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "data loss is our fault, not a 404"
    );
}

#[tokio::test]
async fn must_not_resolve_traversal_paths_and_must_not_ask_authorization_first() {
    let (world, repo) = world_with_hosted().await;
    let alice = principal("alice");
    for bad in [
        "../x",
        "a/../../etc/passwd",
        "a/%2e%2e/b",
        "a//b",
        "a%2fb",
        "a%252e%252e/b",
        "a\\b",
        "a%00b",
    ] {
        let response = world.put(&repo, bad, &alice, CONTENT).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
        let get = world.get(&repo, bad, &alice).await;
        assert_eq!(get.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    assert!(
        world.rbac.checks().is_empty(),
        "classification must refuse before authorization runs"
    );
    assert_eq!(world.blobs.len(), 0);
}

#[tokio::test]
async fn anonymous_callers_are_asked_for_credentials() {
    let (world, repo) = world_with_hosted().await;
    let response = world.get(&repo, "pkg/x.bin", &anonymous()).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = world.put(&repo, "pkg/x.bin", &anonymous(), CONTENT).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(world.blobs.len(), 0);
}

#[tokio::test]
async fn must_not_allow_anonymous_reads_without_the_repository_flag() {
    let world = World::new();
    let repo = world.hosted("files").await;
    // The authorizer would allow anonymous reads, but the repository flag is off.
    world.rbac.grant_read("anonymous", "files");
    let response = world.get(&repo, "pkg/x.bin", &anonymous()).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        world.rbac.checks().is_empty(),
        "without the flag the authorizer is not even consulted"
    );

    let mut open = fixtures::repository(
        "open",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    open.allow_anonymous = true;
    let open = world.add(open).await;
    world.rbac.grant_read("anonymous", "open");
    assert_eq!(
        world.get(&open, "pkg/x.bin", &anonymous()).await.status(),
        StatusCode::NOT_FOUND
    );
    // Anonymous write is refused whatever the flag says (here: not granted).
    assert_eq!(
        world
            .put(&open, "pkg/x.bin", &anonymous(), CONTENT)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_repository_hidden_from_the_caller_answers_404_and_a_visible_one_403() {
    let (world, repo) = world_with_hosted().await;
    world.rbac.grant_read("bob", "files");
    // Bob may read but not write: he can see the repository, so 403.
    assert_eq!(
        world
            .put(&repo, "pkg/x.bin", &principal("bob"), CONTENT)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // Carol has nothing on it: hidden, so 404.
    assert_eq!(
        world
            .get(&repo, "pkg/x.bin", &principal("carol"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn authorization_failure_fails_closed() {
    let (world, repo) = world_with_hosted().await;
    world
        .put(&repo, "pkg/x.bin", &principal("alice"), CONTENT)
        .await;
    world.rbac.set_failing(true);
    let response = world.get(&repo, "pkg/x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    world.rbac.set_failing(false);
    assert_eq!(
        world
            .get(&repo, "pkg/x.bin", &principal("alice"))
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn offline_repository_answers_503() {
    let world = World::new();
    let mut repository = fixtures::repository(
        "off",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Hosted,
    );
    repository.online = false;
    let repo = world.add(repository).await;
    world.allow_all("alice", &[&repo]);
    assert_eq!(
        world
            .get(&repo, "a.bin", &principal("alice"))
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn errors_are_rendered_by_the_format_not_the_generic_envelope() {
    let (world, repo) = world_with_hosted().await;
    let response = world.get(&repo, "missing/x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_text(response).await, "RAW-ERROR not_found");
}

#[tokio::test]
async fn error_bodies_never_contain_internal_detail() {
    let (world, repo) = world_with_hosted().await;
    world.store.set_unavailable(true);
    let mut head = world.head(Method::GET, "pkg/x.bin", &principal("alice"));
    head.client.request_id = Some("req-42".into());
    let response = world.send_with(&repo, head, Body::empty()).await;
    assert_eq!(
        header_of(response.headers(), "x-request-id"),
        Some("req-42")
    );
    let text = body_text(response).await;
    assert!(!text.contains("memory store"), "{text}");
    world.store.set_unavailable(false);
}

#[tokio::test]
async fn unsupported_method_is_405_before_anything_else() {
    let (world, repo) = world_with_hosted().await;
    let response = world
        .send(
            &repo,
            Method::OPTIONS,
            "a/b",
            &principal("alice"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(response.headers().contains_key(header::ALLOW));
}

#[tokio::test]
async fn upload_to_a_non_hosted_repository_is_refused_before_the_body_is_read() {
    let world = World::new();
    let proxy = world.proxy("mirror").await;
    world.allow_all("alice", &[&proxy]);
    let (polled, body) = tracked_body(CONTENT);
    let response = world
        .send(&proxy, Method::PUT, "pkg/x.bin", &principal("alice"), body)
        .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(!polled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_failing_audit_sink_does_not_undo_a_committed_upload() {
    let world = World::with_ports(|ports| ports.audit = std::sync::Arc::new(support::FailingAudit));
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let alice = principal("alice");
    let response = world.put(&repo, "pkg/a.bin", &alice, CONTENT).await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "the audit sink failure is logged, the data is already committed"
    );
    assert_eq!(
        world.get(&repo, "pkg/a.bin", &alice).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_abandoned_writer_is_aborted_even_if_the_store_would_keep_it() {
    let blobs = rustryx_testkit::MemBlobStore::new();
    // Like a crashed process: a dropped writer leaves its staging entry behind.
    blobs.set_keep_abandoned_staging(true);
    let world = World::with_blob_store(blobs.clone());
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let (tx, body) = channel_body();
    let request = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::PUT, "pkg/abandoned.bin", &principal("alice"));
        async move { engine.serve(repo, head, body).await }
    });
    tx.send(Ok(Bytes::from_static(b"bytes"))).await.unwrap();
    eventually("staged", || blobs.staging_count() == 1).await;
    request.abort();
    let _ = request.await;
    eventually("staging aborted by the sink, not by the store", || {
        blobs.staging_count() == 0
    })
    .await;
    drop(tx);
}

#[tokio::test]
async fn an_upload_cancelled_right_after_the_commit_is_still_published_not_orphaned() {
    let (world, flaky) = World::with_flaky_store();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let request = tokio::spawn({
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::PUT, "pkg/atomic.bin", &principal("alice"));
        async move {
            let _ = gate_rx.await;
            engine.serve(repo, head, Body::from(CONTENT)).await.status()
        }
    });
    // The client connection dies in the instant between the blob commit and the
    // metadata commit. Hook runs inside the store's commit.
    let abort = request.abort_handle();
    flaky.after_commit(move || abort.abort());
    gate_tx.send(()).unwrap();
    assert!(request.await.unwrap_err().is_cancelled());

    world.settle().await;
    let asset = world
        .store
        .get_by_path(repo.id, "pkg/atomic.bin")
        .await
        .unwrap();
    assert!(
        asset.is_some(),
        "the finalizer outlives the request, so the committed blob gets its row"
    );
    assert_eq!(flaky.inner.len(), 1);
}
