//! Download announcements and batched counters: who counts as a download, and how the batches
//! reach `AssetRepo::record_downloads`.
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
use http::{HeaderValue, Method, header};
use rustryx_core::identity::AuthMethod;
use rustryx_core::ids::{AssetId, RepositoryId};
use rustryx_core::model::DownloadSource;
use rustryx_core::ports::AssetRepo;
use rustryx_format_core::testing::Reply;
use rustryx_format_core::{ContentEvent, ContentEvents, DownloadBatcher, NoopEvents};
use support::doubles::AssetSpy;
use support::recorder::TestRecorder;
use support::{RecordingEvents, World, body_bytes, principal};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const CONTENT: &[u8] = b"hosted artifact bytes";

fn downloads(world: &World) -> Vec<(AssetId, DownloadSource)> {
    world
        .events
        .events()
        .into_iter()
        .filter_map(|event| match event {
            ContentEvent::AssetDownloaded { asset, source, .. } => Some((asset, source)),
            _ => None,
        })
        .collect()
}

async fn hosted_world() -> (World, Arc<rustryx_core::model::Repository>, AssetId) {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    world.put_as_admin(&repo, "pkg/a.bin", CONTENT).await;
    let asset = world
        .store
        .get_by_path(repo.id, "pkg/a.bin")
        .await
        .unwrap()
        .unwrap();
    (world, repo, asset.id)
}

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000 + seconds).unwrap()
}

fn download(asset: AssetId, source: DownloadSource, seconds: i64) -> ContentEvent {
    ContentEvent::AssetDownloaded {
        repository: RepositoryId::generate(),
        asset,
        source,
        at: at(seconds),
    }
}

#[tokio::test]
async fn a_client_get_announces_one_download_per_read() {
    let (world, repo, asset) = hosted_world().await;
    let alice = principal("alice");
    for expected in 1..=3 {
        assert_eq!(world.get(&repo, "pkg/a.bin", &alice).await.status(), 200);
        assert_eq!(downloads(&world).len(), expected);
    }
    assert!(
        downloads(&world)
            .iter()
            .all(|(id, source)| *id == asset && *source == DownloadSource::Client)
    );
}

#[tokio::test]
async fn only_a_get_that_opens_the_body_is_a_download() {
    let (world, repo, _asset) = hosted_world().await;
    let alice = principal("alice");
    // HEAD: no body.
    let head = world.head(Method::HEAD, "pkg/a.bin", &alice);
    assert_eq!(
        world.send_with(&repo, head, Body::empty()).await.status(),
        200
    );
    assert!(downloads(&world).is_empty(), "a HEAD is not a download");
    // 304: nothing was transferred.
    let etag = world
        .get(&repo, "pkg/a.bin", &alice)
        .await
        .headers()
        .get(header::ETAG)
        .unwrap()
        .clone();
    let before = downloads(&world).len();
    let mut conditional = world.head(Method::GET, "pkg/a.bin", &alice);
    conditional.headers.insert(header::IF_NONE_MATCH, etag);
    assert_eq!(
        world
            .send_with(&repo, conditional, Body::empty())
            .await
            .status(),
        304
    );
    assert_eq!(downloads(&world).len(), before, "a 304 is not a download");
    // A range read opens the body, so it counts.
    let mut ranged = world.head(Method::GET, "pkg/a.bin", &alice);
    ranged
        .headers
        .insert(header::RANGE, HeaderValue::from_static("bytes=0-3"));
    assert_eq!(
        world.send_with(&repo, ranged, Body::empty()).await.status(),
        206
    );
    assert_eq!(downloads(&world).len(), before + 1);
}

#[tokio::test]
async fn an_index_document_read_is_not_a_download() {
    let world = World::new();
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    world
        .put_as_admin(&repo, "pkg/list.index", b"one\ntwo")
        .await;
    assert_eq!(
        world
            .get(&repo, "pkg/list.index", &principal("alice"))
            .await
            .status(),
        200
    );
    assert!(
        downloads(&world).is_empty(),
        "a group merge reads one index per member: metadata is not a pull of the component"
    );
}

#[tokio::test]
async fn must_not_count_scanner_reads() {
    let (world, repo, _asset) = hosted_world().await;
    world.allow_all("scanner", &[&repo]);
    let mut scanner = principal("scanner");
    scanner.auth_method = AuthMethod::ScanToken;
    let response = world.get(&repo, "pkg/a.bin", &scanner).await;
    assert_eq!(response.status(), 200);
    assert_eq!(&body_bytes(response).await[..], CONTENT);
    assert!(
        downloads(&world).is_empty(),
        "a scanner that walks every artifact must not keep an old one alive"
    );
}

#[tokio::test]
async fn must_not_count_replication_or_internal_reads() {
    let (world, repo, _asset) = hosted_world().await;
    let alice = principal("alice");
    for source in [DownloadSource::Replication, DownloadSource::Internal] {
        let mut head = world.head(Method::GET, "pkg/a.bin", &alice);
        head.client.download_source = Some(source);
        assert_eq!(
            world.send_with(&repo, head, Body::empty()).await.status(),
            200
        );
    }
    assert_eq!(
        downloads(&world).len(),
        0,
        "replication and internal reads never count"
    );
    // And an explicit `Client` counts like the default.
    let mut head = world.head(Method::GET, "pkg/a.bin", &alice);
    head.client.download_source = Some(DownloadSource::Client);
    world.send_with(&repo, head, Body::empty()).await;
    assert_eq!(downloads(&world).len(), 1);
}

#[tokio::test]
async fn a_cached_proxy_read_counts_but_the_first_client_of_a_miss_does_not() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.allow_all("alice", &[&repo]);
    world.upstream.route("/r/a.bin", vec![Reply::ok(CONTENT)]);
    let alice = principal("alice");
    body_bytes(world.get(&repo, "r/a.bin", &alice).await).await;
    world.settle().await;
    assert!(
        downloads(&world).is_empty(),
        "the first client was served from the tee, the copy did not exist yet"
    );
    assert_eq!(world.get(&repo, "r/a.bin", &alice).await.status(), 200);
    assert_eq!(downloads(&world).len(), 1);
    // A HEAD of the cached copy is not a download either.
    let head = world.head(Method::HEAD, "r/a.bin", &alice);
    world.send_with(&repo, head, Body::empty()).await;
    assert_eq!(downloads(&world).len(), 1);
}

#[tokio::test]
async fn a_group_read_counts_the_asset_of_the_member_that_served_it() {
    let world = World::new();
    let local = world.hosted("local").await;
    let group = world.group("all", &[&local]).await;
    world.allow_all("alice", &[&local, &group]);
    world.put_as_admin(&local, "pkg/a.bin", CONTENT).await;
    let asset = world
        .store
        .get_by_path(local.id, "pkg/a.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        world
            .get(&group, "pkg/a.bin", &principal("alice"))
            .await
            .status(),
        200
    );
    let seen = downloads(&world);
    assert_eq!(seen, [(asset.id, DownloadSource::Client)]);
}

fn batcher(cap: usize) -> (Arc<DownloadBatcher>, Arc<RecordingEvents>) {
    let inner = Arc::new(RecordingEvents::default());
    (DownloadBatcher::new(inner.clone(), cap), inner)
}

fn spy_world() -> (World, Arc<AssetSpy>) {
    let spy = AssetSpy::new();
    let world = World::with_ports(|ports| spy.install(ports));
    (world, spy)
}

#[tokio::test]
async fn the_batcher_folds_events_into_one_tick_per_asset() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(100);
    let (a, b) = (AssetId::generate(), AssetId::generate());
    for seconds in [5, 20, 10] {
        batcher.emit(download(a, DownloadSource::Client, seconds));
    }
    batcher.emit(download(b, DownloadSource::Client, 7));
    assert_eq!(batcher.pending(), 2);
    assert_eq!(recorder.gauge("download_counter_pending", &[]), Some(2.0));

    assert_eq!(batcher.flush(spy.as_ref()).await.unwrap(), 2);
    let batches = spy.download_batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 1, "one call for the whole map");
    let tick = |id: AssetId| {
        batches[0]
            .iter()
            .find(|t| t.asset_id == id)
            .copied()
            .unwrap()
    };
    assert_eq!(
        (tick(a).count, tick(a).at),
        (3, at(20)),
        "the latest time wins"
    );
    assert_eq!((tick(b).count, tick(b).at), (1, at(7)));
    assert_eq!(batcher.pending(), 0);
    assert_eq!(recorder.gauge("download_counter_pending", &[]), Some(0.0));
    assert_eq!(
        recorder.counter("download_counter_flush_total", &[("result", "ok")]),
        1
    );
}

#[tokio::test]
async fn must_not_count_an_event_that_is_not_a_client_download() {
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(100);
    let asset = AssetId::generate();
    for source in [
        DownloadSource::Scanner,
        DownloadSource::Replication,
        DownloadSource::Internal,
    ] {
        batcher.emit(download(asset, source, 1));
    }
    assert_eq!(batcher.pending(), 0);
    assert_eq!(batcher.flush(spy.as_ref()).await.unwrap(), 0);
    assert_eq!(
        spy.record_calls.load(Ordering::SeqCst),
        0,
        "nothing pending, nothing written"
    );
}

#[tokio::test]
async fn the_batcher_forwards_every_event_to_the_sink_behind_it() {
    let (batcher, inner) = batcher(100);
    batcher.emit(download(AssetId::generate(), DownloadSource::Scanner, 1));
    batcher.emit(download(AssetId::generate(), DownloadSource::Client, 2));
    batcher.emit(ContentEvent::BlobOrphaned {
        repository: RepositoryId::generate(),
        blob: rustryx_testkit::fixtures::blob_record("k", b"x"),
        cause: rustryx_format_core::OrphanCause::Deleted,
    });
    assert_eq!(
        inner.events().len(),
        3,
        "the batcher only adds, it never swallows"
    );
}

#[tokio::test]
async fn must_not_double_count_after_a_failed_flush() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(100);
    let asset = AssetId::generate();
    for seconds in 1..=3 {
        batcher.emit(download(asset, DownloadSource::Client, seconds));
    }
    spy.record_downloads_fault.store(true, Ordering::SeqCst);
    assert!(batcher.flush(spy.as_ref()).await.is_err());
    assert_eq!(
        recorder.counter("download_counter_flush_total", &[("result", "error")]),
        1
    );
    assert_eq!(
        batcher.pending(),
        1,
        "the counts are kept for the next flush"
    );

    // Two more arrive while the store is still down, then it recovers.
    batcher.emit(download(asset, DownloadSource::Client, 10));
    batcher.emit(download(asset, DownloadSource::Client, 11));
    spy.record_downloads_fault.store(false, Ordering::SeqCst);
    assert_eq!(batcher.flush(spy.as_ref()).await.unwrap(), 1);
    let batches = spy.download_batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0][0].count, 5, "kept and new counts are one tick");
    assert_eq!(batches[0][0].at, at(11));
    // A batch that was written is gone: a third flush sends nothing.
    assert_eq!(batcher.flush(spy.as_ref()).await.unwrap(), 0);
    assert_eq!(spy.download_batches.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn must_not_grow_the_counter_map_beyond_its_cap() {
    let recorder = TestRecorder::new();
    let _guard = recorder.install();
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(2);
    let (a, b, c) = (
        AssetId::generate(),
        AssetId::generate(),
        AssetId::generate(),
    );
    batcher.emit(download(a, DownloadSource::Client, 1));
    batcher.emit(download(b, DownloadSource::Client, 2));
    batcher.emit(download(c, DownloadSource::Client, 3));
    batcher.emit(download(a, DownloadSource::Client, 4));
    assert_eq!(
        batcher.pending(),
        2,
        "a new asset beyond the cap is dropped"
    );
    assert_eq!(recorder.counter("download_counter_dropped_total", &[]), 1);
    batcher.flush(spy.as_ref()).await.unwrap();
    let batches = spy.download_batches.lock().unwrap().clone();
    let tick = |id: AssetId| batches[0].iter().find(|t| t.asset_id == id).copied();
    assert_eq!(
        tick(a).unwrap().count,
        2,
        "a tracked asset keeps counting at the cap"
    );
    assert!(tick(c).is_none());
}

#[tokio::test]
async fn a_failed_chunk_keeps_it_and_the_rest_but_not_what_was_written() {
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(100_000);
    for _ in 0..5_001 {
        batcher.emit(download(AssetId::generate(), DownloadSource::Client, 1));
    }
    // 5 001 assets are two calls of at most 5 000. The second one fails.
    spy.fail_record_call.store(2, Ordering::SeqCst);
    assert!(batcher.flush(spy.as_ref()).await.is_err());
    assert_eq!(
        batcher.pending(),
        1,
        "the 5 000 that were written must not come back, the one that was not must stay"
    );
    spy.fail_record_call.store(0, Ordering::SeqCst);
    assert_eq!(batcher.flush(spy.as_ref()).await.unwrap(), 1);
    let sizes: Vec<usize> = spy
        .download_batches
        .lock()
        .unwrap()
        .iter()
        .map(Vec::len)
        .collect();
    assert_eq!(sizes, [5_000, 1]);

    // A failure of the first chunk keeps everything.
    for _ in 0..5_001 {
        batcher.emit(download(AssetId::generate(), DownloadSource::Client, 1));
    }
    spy.record_calls.store(0, Ordering::SeqCst);
    spy.fail_record_call.store(1, Ordering::SeqCst);
    assert!(batcher.flush(spy.as_ref()).await.is_err());
    assert_eq!(batcher.pending(), 5_001);
}

#[tokio::test]
async fn an_empty_flush_writes_nothing() {
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(10);
    assert_eq!(batcher.flush(spy.as_ref()).await.unwrap(), 0);
    assert_eq!(spy.record_calls.load(Ordering::SeqCst), 0);
    assert!(format!("{batcher:?}").contains("DownloadBatcher"));
}

#[tokio::test]
async fn the_engine_feeds_the_batcher_and_a_flush_writes_the_counts() {
    let spy = AssetSpy::new();
    let (batcher, _inner) = batcher(100);
    let world = World::with_ports(|ports| {
        spy.install(ports);
        ports.events = batcher.clone();
    });
    let repo = world.hosted("files").await;
    world.allow_all("alice", &[&repo]);
    world.put_as_admin(&repo, "pkg/a.bin", CONTENT).await;
    let alice = principal("alice");
    world.get(&repo, "pkg/a.bin", &alice).await;
    world.get(&repo, "pkg/a.bin", &alice).await;
    assert_eq!(batcher.pending(), 1);
    batcher.flush(spy.as_ref()).await.unwrap();
    let batches = spy.download_batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 1);
    assert_eq!(batches[0][0].count, 2);
}

#[tokio::test(start_paused = true)]
async fn the_flusher_flushes_every_interval_retries_after_a_failure_and_flushes_at_shutdown() {
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(100);
    let tracker = TaskTracker::new();
    let shutdown = CancellationToken::new();
    batcher.spawn_flusher(
        spy.clone() as Arc<dyn AssetRepo>,
        Duration::from_secs(10),
        shutdown.clone(),
        &tracker,
    );
    let asset = AssetId::generate();

    // Nothing is written before the first interval has passed.
    batcher.emit(download(asset, DownloadSource::Client, 1));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(spy.record_calls.load(Ordering::SeqCst), 0);

    // The first tick finds the store down: the counts stay.
    spy.record_downloads_fault.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(spy.record_calls.load(Ordering::SeqCst), 1);
    assert_eq!(batcher.pending(), 1);

    // The next one retries them together with what arrived since.
    spy.record_downloads_fault.store(false, Ordering::SeqCst);
    batcher.emit(download(asset, DownloadSource::Client, 2));
    tokio::time::sleep(Duration::from_secs(10)).await;
    let batches = spy.download_batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0][0].count, 2);

    // Shutdown writes the rest.
    batcher.emit(download(asset, DownloadSource::Client, 3));
    shutdown.cancel();
    tracker.close();
    tracker.wait().await;
    let batches = spy.download_batches.lock().unwrap().clone();
    assert_eq!(
        batches.len(),
        2,
        "the final flush runs before the task ends"
    );
    assert_eq!(batches[1][0].count, 1);
    assert_eq!(batcher.pending(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_flusher_that_cannot_write_at_shutdown_does_not_hang() {
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(100);
    let tracker = TaskTracker::new();
    let shutdown = CancellationToken::new();
    batcher.spawn_flusher(
        spy.clone() as Arc<dyn AssetRepo>,
        Duration::from_secs(10),
        shutdown.clone(),
        &tracker,
    );
    batcher.emit(download(AssetId::generate(), DownloadSource::Client, 1));
    spy.record_downloads_fault.store(true, Ordering::SeqCst);
    shutdown.cancel();
    tracker.close();
    tracker.wait().await;
    assert_eq!(
        batcher.pending(),
        1,
        "the counts that could not be written are lost with the process, not hung on"
    );
}

#[tokio::test(start_paused = true)]
async fn a_final_flush_that_hangs_is_cut_off_so_the_shutdown_completes() {
    let (_world, spy) = spy_world();
    let (batcher, _inner) = batcher(100);
    let tracker = TaskTracker::new();
    let shutdown = CancellationToken::new();
    batcher.spawn_flusher(
        spy.clone() as Arc<dyn AssetRepo>,
        Duration::from_secs(10),
        shutdown.clone(),
        &tracker,
    );
    batcher.emit(download(AssetId::generate(), DownloadSource::Client, 1));
    spy.record_downloads_hang.store(true, Ordering::SeqCst);
    shutdown.cancel();
    tracker.close();
    // Paused time advances by itself while everything is idle: this returns only because the
    // flusher gave up on the hung write.
    tokio::time::timeout(Duration::from_secs(60), tracker.wait())
        .await
        .expect("the flusher must not hold the shutdown");
}

#[test]
fn a_noop_sink_accepts_the_events_the_batcher_forwards_to_it() {
    let batcher = DownloadBatcher::new(Arc::new(NoopEvents), 1);
    batcher.emit(download(AssetId::generate(), DownloadSource::Client, 1));
    assert_eq!(batcher.pending(), 1);
}
