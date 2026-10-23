//! Chunked uploads on stores with the resumable capability (the pipeline hook OCI blob uploads
//! will use): bounded memory, the same limits and the same ending as a single request upload.
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
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum_core::body::Body;
use bytes::Bytes;
use http::Method;
use rustryx_core::error::{BlobError, PolicyViolation};
use rustryx_core::ids::{ImmutabilityRuleId, RepositoryId};
use rustryx_core::model::{RepoFormat, RepoType, Repository, WritePolicy};
use rustryx_core::ports::AssetRepo;
use rustryx_format_core::{
    ChunkedSession, ExpectedChecksum, FormatContext, FormatError, HashAlgo, Limits, NormalizedPath,
    UploadSpec,
};
use rustryx_testkit::{MemBlobStore, fixtures};
use support::doubles::{ScriptedPolicy, SpyStore, Verdict};
use support::{World, body_bytes, channel_body, principal, tracked_body};

fn path(text: &str) -> NormalizedPath {
    NormalizedPath::parse(text).unwrap()
}

fn hosted(name: &str, policy: WritePolicy) -> Repository {
    let mut repository = fixtures::repository(name, RepoFormat::Raw, RepoType::Hosted);
    repository.write_policy = policy;
    repository
}

struct Fixture {
    world: World,
    store: Arc<SpyStore>,
    repo: Arc<Repository>,
}

async fn fixture_with(limits: Limits, policy: WritePolicy) -> Fixture {
    let blobs = MemBlobStore::new();
    let store = SpyStore::resumable(blobs.clone());
    let world = World::with_store_and_limits(store.clone(), blobs, limits);
    let repo = world.add(hosted("blobs", policy)).await;
    world.allow_all("alice", &[&repo]);
    Fixture { world, store, repo }
}

async fn fixture() -> Fixture {
    fixture_with(Limits::default(), WritePolicy::Allow).await
}

impl Fixture {
    fn cx(&self) -> FormatContext {
        let head = self
            .world
            .head(Method::PATCH, "blobs/uploads", &principal("alice"));
        self.world.engine.context(self.repo.clone(), &head)
    }

    fn body(data: &'static [u8]) -> Body {
        Body::from(Bytes::from_static(data))
    }
}

#[tokio::test]
async fn supports_chunked_follows_the_capability_of_the_store() {
    let f = fixture().await;
    assert!(f.cx().hosted().supports_chunked().await.unwrap());

    let blobs = MemBlobStore::new();
    let plain = SpyStore::plain(blobs.clone());
    let world = World::with_store(plain, blobs);
    let repo = world.add(hosted("plain", WritePolicy::Allow)).await;
    let head = world.head(Method::PATCH, "x", &principal("alice"));
    let cx = world.engine.context(repo, &head);
    assert!(!cx.hosted().supports_chunked().await.unwrap());
    let error = cx.hosted().begin_chunked().await.unwrap_err();
    assert!(
        matches!(
            error,
            FormatError::Blob(BlobError::Unsupported("resumable"))
        ),
        "{error:?}"
    );
    assert_eq!(error.status(), 500, "a caller that did not probe has a bug");
}

#[tokio::test]
async fn a_chunked_upload_round_trips_across_requests_and_nodes() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    assert_eq!(cx.hosted().chunked_length(&session).await.unwrap(), 0);

    // The first PATCH is served by this node, the second by another one: all the state is in
    // the token and in the store.
    let length = cx
        .hosted()
        .append_chunk(&session, 0, Fixture::body(b"hello "), Some(6))
        .await
        .unwrap();
    assert_eq!(length, 6);
    let token = session.token();
    let other_node = f.world.second_node();
    let head = f
        .world
        .head(Method::PATCH, "blobs/uploads", &principal("alice"));
    let other_cx = other_node.context(f.repo.clone(), &head);
    let resumed = ChunkedSession::parse(&token, f.repo.id).unwrap();
    assert_eq!(resumed, session);
    assert_eq!(other_cx.hosted().chunked_length(&resumed).await.unwrap(), 6);
    let length = other_cx
        .hosted()
        .append_chunk(&resumed, 6, Fixture::body(b"world"), None)
        .await
        .unwrap();
    assert_eq!(length, 11);

    let outcome = other_cx
        .hosted()
        .finish_chunked(resumed, UploadSpec::new(path("blobs/greeting")))
        .await
        .unwrap();
    assert!(outcome.created());
    assert_eq!(outcome.asset.size_bytes, 11);
    assert_eq!(f.store.finalizes.load(Ordering::SeqCst), 1);
    assert_eq!(f.world.blobs.len(), 1);
    assert_eq!(f.world.blobs.open_uploads(), 0);
    assert_eq!(
        outcome.asset.checksums,
        fixtures::checksums_of(b"hello world")
    );
    let response = f
        .world
        .get(&f.repo, "blobs/greeting", &principal("alice"))
        .await;
    assert_eq!(&body_bytes(response).await[..], b"hello world");
    let actions: Vec<_> = f
        .world
        .audit
        .events()
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert_eq!(actions, ["asset.publish"]);
    other_node.drain().await;
}

#[tokio::test]
async fn must_not_buffer_a_patch_body_beyond_one_segment() {
    let limits = Limits {
        chunk_segment_bytes: 4,
        ..Limits::default()
    };
    let f = fixture_with(limits, WritePolicy::Allow).await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    // Ten bytes arrive as ten one byte frames.
    let (tx, body) = channel_body();
    let append = tokio::spawn({
        let cx = cx.clone();
        let session = session.clone();
        async move { cx.hosted().append_chunk(&session, 0, body, Some(10)).await }
    });
    for byte in b"0123456789" {
        tx.send(Ok(Bytes::copy_from_slice(&[*byte]))).await.unwrap();
    }
    drop(tx);
    assert_eq!(append.await.unwrap().unwrap(), 10);
    let appends = f.store.appends.lock().unwrap().clone();
    assert_eq!(
        appends,
        [(0, 4), (4, 4), (8, 2)],
        "each staged chunk is at most one segment and the offsets follow the length"
    );
}

#[tokio::test]
async fn a_wrong_offset_reports_the_staged_length() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"12345"), None)
        .await
        .unwrap();
    let error = cx
        .hosted()
        .append_chunk(&session, 3, Fixture::body(b"xx"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, FormatError::RangeNotSatisfiable { total: 5 }),
        "the client resumes from the length the store has: {error:?}"
    );
    assert_eq!(error.status(), 416);
    assert_eq!(
        error
            .headers()
            .get(http::header::CONTENT_RANGE)
            .unwrap()
            .to_str()
            .unwrap(),
        "bytes */5"
    );
    assert_eq!(cx.hosted().chunked_length(&session).await.unwrap(), 5);
}

#[tokio::test]
async fn must_not_stage_a_chunk_over_the_blob_limit() {
    let limits = Limits {
        max_blob_bytes: 10,
        ..Limits::default()
    };
    let f = fixture_with(limits, WritePolicy::Allow).await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"12345678"), None)
        .await
        .unwrap();
    // Eight bytes are staged: five more would make 13.
    let error = cx
        .hosted()
        .append_chunk(&session, 8, Fixture::body(b"abcde"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, FormatError::PayloadTooLarge { limit: 10 }),
        "{error:?}"
    );
    assert_eq!(
        cx.hosted().chunked_length(&session).await.unwrap(),
        8,
        "nothing of the refused chunk was staged"
    );
}

#[tokio::test]
async fn must_not_read_a_chunk_body_when_the_declared_length_exceeds_the_room() {
    let limits = Limits {
        max_blob_bytes: 10,
        ..Limits::default()
    };
    let f = fixture_with(limits, WritePolicy::Allow).await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    let (polled, body) = tracked_body(b"0123456789abcdef");
    let error = cx
        .hosted()
        .append_chunk(&session, 4, body, Some(16))
        .await
        .unwrap_err();
    assert!(
        matches!(error, FormatError::PayloadTooLarge { .. }),
        "{error:?}"
    );
    assert!(!polled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_chunk_longer_than_its_declared_length_is_refused_and_not_staged() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    let error = cx
        .hosted()
        .append_chunk(&session, 0, Fixture::body(b"too long"), Some(3))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::BadRequest(_)), "{error:?}");
    assert_eq!(cx.hosted().chunked_length(&session).await.unwrap(), 0);
    // And one that is shorter than it declared.
    let error = cx
        .hosted()
        .append_chunk(&session, 0, Fixture::body(b"ab"), Some(5))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::BadRequest(_)), "{error:?}");
}

#[tokio::test]
async fn a_stalled_chunk_body_times_out_and_keeps_what_was_staged() {
    let limits = Limits {
        body_idle_timeout: Duration::from_millis(100),
        chunk_segment_bytes: 2,
        ..Limits::default()
    };
    let f = fixture_with(limits, WritePolicy::Allow).await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    let (tx, body) = channel_body();
    let append = tokio::spawn({
        let cx = cx.clone();
        let session = session.clone();
        async move { cx.hosted().append_chunk(&session, 0, body, None).await }
    });
    tx.send(Ok(Bytes::from_static(b"abcd"))).await.unwrap();
    let error = append.await.unwrap().unwrap_err();
    assert!(matches!(error, FormatError::RequestTimeout), "{error:?}");
    drop(tx);
    assert_eq!(
        cx.hosted().chunked_length(&session).await.unwrap(),
        4,
        "the staged chunks stay: the client resumes from the reported length"
    );
}

#[tokio::test]
async fn a_body_that_breaks_is_a_bad_request() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    let (tx, body) = channel_body();
    let append = tokio::spawn({
        let cx = cx.clone();
        let session = session.clone();
        async move { cx.hosted().append_chunk(&session, 0, body, None).await }
    });
    tx.send(Err(std::io::Error::other("connection reset")))
        .await
        .unwrap();
    let error = append.await.unwrap().unwrap_err();
    assert!(matches!(error, FormatError::BadRequest(_)), "{error:?}");
}

#[tokio::test]
async fn finish_runs_the_write_policy_like_a_single_upload() {
    let f = fixture_with(Limits::default(), WritePolicy::AllowOnce).await;
    let cx = f.cx();
    assert!(
        cx.hosted()
            .upload(
                UploadSpec::new(path("blobs/taken")),
                Fixture::body(b"original")
            )
            .await
            .is_ok()
    );
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"replacement"), None)
        .await
        .unwrap();
    let error = cx
        .hosted()
        .finish_chunked(session.clone(), UploadSpec::new(path("blobs/taken")))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::Conflict(_)), "{error:?}");
    assert_eq!(
        f.store.finalizes.load(Ordering::SeqCst),
        0,
        "a refused finish must not even join the chunks"
    );
    // The session is still there, so the client can abort it.
    cx.hosted().abort_chunked(session).await.unwrap();
    assert_eq!(f.world.blobs.open_uploads(), 0);
    assert_eq!(f.world.blobs.len(), 1, "only the original blob exists");
}

#[tokio::test]
async fn the_policy_hook_sees_the_real_total_at_finish() {
    let policy = ScriptedPolicy::allowing();
    let blobs = MemBlobStore::new();
    let store = SpyStore::resumable(blobs.clone());
    let world = World::with_store_and_policy(store, blobs, policy.clone());
    let repo = world.add(hosted("blobs", WritePolicy::Allow)).await;
    world.allow_all("alice", &[&repo]);
    let head = world.head(Method::PATCH, "x", &principal("alice"));
    let cx = world.engine.context(repo, &head);
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"abc"), None)
        .await
        .unwrap();
    cx.hosted()
        .append_chunk(&session, 3, Fixture::body(b"defg"), None)
        .await
        .unwrap();
    assert!(
        policy.writes.lock().unwrap().is_empty(),
        "there is no path and no size at the start of a chunked upload"
    );
    cx.hosted()
        .finish_chunked(session, UploadSpec::new(path("blobs/x")))
        .await
        .unwrap();
    let seen = policy.writes.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "blobs/x");
    assert_eq!(seen[0].declared_length, Some(7));
}

#[tokio::test]
async fn a_policy_refusal_at_finish_stops_before_the_chunks_are_joined() {
    let policy = ScriptedPolicy::new(
        Verdict::Refuse(PolicyViolation::Immutable {
            rule: ImmutabilityRuleId::generate(),
        }),
        Verdict::Allow,
    );
    let blobs = MemBlobStore::new();
    let store = SpyStore::resumable(blobs.clone());
    let world = World::with_store_and_policy(store.clone(), blobs, policy);
    let repo = world.add(hosted("blobs", WritePolicy::Allow)).await;
    let head = world.head(Method::PATCH, "x", &principal("alice"));
    let cx = world.engine.context(repo, &head);
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"abc"), None)
        .await
        .unwrap();
    let error = cx
        .hosted()
        .finish_chunked(session, UploadSpec::new(path("blobs/x")))
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::Policy(_)), "{error:?}");
    assert_eq!(store.finalizes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_digest_mismatch_at_finish_deletes_the_committed_blob() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"payload"), None)
        .await
        .unwrap();
    let mut spec = UploadSpec::new(path("blobs/x"));
    spec.expected = Some(ExpectedChecksum::new(HashAlgo::Sha256, &"0".repeat(64)).unwrap());
    let error = cx.hosted().finish_chunked(session, spec).await.unwrap_err();
    assert!(matches!(error, FormatError::DigestMismatch), "{error:?}");
    assert_eq!(
        f.world.blobs.len(),
        0,
        "the joined object must not stay behind"
    );
    assert_eq!(f.store.finalizes.load(Ordering::SeqCst), 1);
    let events = f.world.audit.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "asset.rejected");
}

#[tokio::test]
async fn a_matching_digest_publishes() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"payload"), None)
        .await
        .unwrap();
    let mut spec = UploadSpec::new(path("blobs/x"));
    let sha = fixtures::checksums_of(b"payload").sha256;
    spec.expected = Some(ExpectedChecksum::new(HashAlgo::Sha256, &sha).unwrap());
    spec.content_length = Some(7);
    assert!(cx.hosted().finish_chunked(session, spec).await.is_ok());
}

#[tokio::test]
async fn finish_checks_the_declared_length_against_the_staged_length() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"payload"), None)
        .await
        .unwrap();
    let mut spec = UploadSpec::new(path("blobs/x"));
    spec.content_length = Some(99);
    let error = cx.hosted().finish_chunked(session, spec).await.unwrap_err();
    assert!(matches!(error, FormatError::BadRequest(_)), "{error:?}");
    assert_eq!(f.store.finalizes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_failing_finalize_publishes_nothing() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"payload"), None)
        .await
        .unwrap();
    f.store.fail_finalize.store(true, Ordering::SeqCst);
    let error = cx
        .hosted()
        .finish_chunked(session, UploadSpec::new(path("blobs/x")))
        .await
        .unwrap_err();
    assert_eq!(error.status(), 503, "{error:?}");
    assert_eq!(f.world.blobs.len(), 0);
    assert!(
        f.world
            .store
            .get_by_path(f.repo.id, "blobs/x")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_finish_cancelled_in_the_middle_of_the_join_still_publishes() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted()
        .append_chunk(&session, 0, Fixture::body(b"payload"), None)
        .await
        .unwrap();
    *f.store.finalize_delay.lock().unwrap() = Duration::from_millis(100);
    // The client goes away while the node is joining the chunks.
    let outcome = tokio::time::timeout(
        Duration::from_millis(20),
        cx.hosted()
            .finish_chunked(session, UploadSpec::new(path("blobs/x"))),
    )
    .await;
    assert!(outcome.is_err(), "the request future was dropped");
    f.world.settle().await;
    assert_eq!(
        f.world.blobs.len(),
        1,
        "the finalizer ran to the end in its own task"
    );
    let response = f.world.get(&f.repo, "blobs/x", &principal("alice")).await;
    assert_eq!(&body_bytes(response).await[..], b"payload");
}

#[tokio::test]
async fn a_token_names_its_repository_and_nothing_else_parses() {
    let id = RepositoryId::generate();
    let f = fixture().await;
    let session = f.cx().hosted().begin_chunked().await.unwrap();
    let token = session.token();
    assert!(ChunkedSession::parse(&token, f.repo.id).is_ok());
    assert!(
        matches!(
            ChunkedSession::parse(&token, id),
            Err(FormatError::NotFound)
        ),
        "a session of one repository must not be finished through another"
    );
    assert_eq!(session.to_string(), token);
    let blob = session.blob_id().to_string();
    for bad in [
        "",
        "x",
        "a.b",
        "a.b.c",
        &format!("{}.{blob}", f.repo.id),
        &format!("{}.not-a-uuid.handle", f.repo.id),
        &format!("not-a-uuid.{blob}.handle"),
        &format!("{}.{blob}.", f.repo.id),
        &format!("{}.{blob}.bad/handle", f.repo.id),
    ] {
        assert!(
            matches!(
                ChunkedSession::parse(bad, f.repo.id),
                Err(FormatError::NotFound)
            ),
            "{bad:?}"
        );
    }
}

#[tokio::test]
async fn a_session_cannot_be_used_on_another_repository_even_when_built_by_hand() {
    let f = fixture().await;
    let session = f.cx().hosted().begin_chunked().await.unwrap();
    let other = f.world.add(hosted("other", WritePolicy::Allow)).await;
    f.world.allow_all("alice", &[&other]);
    let head = f.world.head(Method::PATCH, "x", &principal("alice"));
    let other_cx = f.world.engine.context(other, &head);
    let error = other_cx
        .hosted()
        .chunked_length(&session)
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::NotFound), "{error:?}");
    let error = other_cx
        .hosted()
        .append_chunk(&session, 0, Fixture::body(b"x"), None)
        .await
        .unwrap_err();
    assert!(matches!(error, FormatError::NotFound), "{error:?}");
}

#[tokio::test]
async fn abort_is_idempotent_and_an_unknown_session_is_not_found() {
    let f = fixture().await;
    let cx = f.cx();
    let session = cx.hosted().begin_chunked().await.unwrap();
    cx.hosted().abort_chunked(session.clone()).await.unwrap();
    cx.hosted().abort_chunked(session.clone()).await.unwrap();
    assert_eq!(f.store.aborts.load(Ordering::SeqCst), 2);
    let error = cx.hosted().chunked_length(&session).await.unwrap_err();
    assert!(
        matches!(error, FormatError::Blob(BlobError::NotFound)),
        "{error:?}"
    );
    let error = cx
        .hosted()
        .finish_chunked(session, UploadSpec::new(path("blobs/x")))
        .await
        .unwrap_err();
    assert_eq!(error.status(), 404, "{error:?}");
}

#[tokio::test]
async fn begin_is_refused_for_a_read_only_repository_and_for_proxies_and_groups() {
    let f = fixture_with(Limits::default(), WritePolicy::Deny).await;
    let error = f.cx().hosted().begin_chunked().await.unwrap_err();
    assert!(matches!(error, FormatError::Forbidden), "{error:?}");

    let f = fixture().await;
    let proxy = f
        .world
        .add(fixtures::repository(
            "mirror",
            RepoFormat::Raw,
            RepoType::Proxy,
        ))
        .await;
    let head = f.world.head(Method::PATCH, "x", &principal("alice"));
    let cx = f.world.engine.context(proxy, &head);
    let error = cx.hosted().begin_chunked().await.unwrap_err();
    assert!(
        matches!(error, FormatError::MethodNotAllowed { .. }),
        "{error:?}"
    );
}
