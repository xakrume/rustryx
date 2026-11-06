//! Behaviour of the S3 store against an in-memory object store and a fake multipart
//! API. No network: the real S3 dialect is exercised by `tests/s3_seaweed.rs`.

use std::sync::Mutex;

use bytes::Bytes;
use futures::TryStreamExt;
use object_store::memory::InMemory;
use rustryx_core::ports::BlobStore;

use super::*;
use crate::hashing::BlobSink;

mod listing;
mod presigning;
mod support;
use support::*;

#[tokio::test]
async fn small_object_uses_a_single_put_and_no_multipart() {
    let fx = fixture();
    let data = pattern(PART - 1);
    let commit = put(&fx, "small", &data, 100).await;
    assert_eq!(commit, digest(&data));
    assert_eq!(
        fx.fake.snap().created,
        0,
        "no multipart upload for one-part objects"
    );
    assert_eq!(read_all(&fx.store, "small", None).await, data.to_vec());
}

#[tokio::test]
async fn object_of_exactly_one_part_is_still_a_single_put() {
    let fx = fixture();
    let data = pattern(PART);
    put(&fx, "exact", &data, 333).await;
    assert_eq!(fx.fake.snap().created, 0);
    assert_eq!(read_all(&fx.store, "exact", None).await, data.to_vec());
}

#[tokio::test]
async fn empty_object_round_trips() {
    let fx = fixture();
    let commit = put(&fx, "empty", &Bytes::new(), 10).await;
    assert_eq!(commit.size_bytes, 0);
    assert_eq!(
        fx.store
            .head(&key("empty"))
            .await
            .unwrap()
            .unwrap()
            .size_bytes,
        0
    );
    assert_empty(&read_all(&fx.store, "empty", None).await);
}

#[tokio::test]
async fn multipart_upload_splits_into_part_sized_pieces() {
    let fx = fixture();
    let data = pattern(PART * 3 + 123);
    let commit = put(&fx, "big", &data, 777).await;
    assert_eq!(commit, digest(&data));
    let state = fx.fake.snap();
    assert_eq!(state.created, 1);
    assert_eq!(state.part_sizes, vec![PART, PART, PART, 123]);
    assert_eq!(read_all(&fx.store, "big", None).await, data.to_vec());
}

#[tokio::test]
async fn exact_multiple_of_part_size_has_no_empty_trailing_part() {
    let fx = fixture();
    let data = pattern(PART * 2);
    put(&fx, "twice", &data, 500).await;
    assert_eq!(fx.fake.snap().part_sizes, vec![PART, PART]);
    assert_eq!(read_all(&fx.store, "twice", None).await, data.to_vec());
}

#[tokio::test]
async fn must_not_make_the_object_visible_before_commit() {
    let fx = fixture();
    let data = pattern(PART * 2 + 10);
    let mut writer = fx.store.begin(&key("hidden"), None).await.unwrap();
    for piece in data.chunks(400) {
        writer.write(Bytes::copy_from_slice(piece)).await.unwrap();
    }
    // Two parts are already stored remotely, yet nothing is addressable by key.
    assert!(fx.fake.snap().part_sizes.len() >= 2);
    assert!(fx.store.head(&key("hidden")).await.unwrap().is_none());
    assert!(matches!(
        fx.store.get(&key("hidden"), None).await,
        Err(BlobError::NotFound)
    ));
    assert_empty(&fx.target.list(None).try_collect::<Vec<_>>().await.unwrap());

    writer.commit().await.unwrap();
    assert_eq!(
        fx.store
            .head(&key("hidden"))
            .await
            .unwrap()
            .unwrap()
            .size_bytes,
        data.len() as u64
    );
}

#[tokio::test]
async fn must_not_leave_an_upload_behind_after_abort() {
    let fx = fixture();
    let mut writer = fx.store.begin(&key("aborted"), None).await.unwrap();
    writer.write(pattern(PART * 2 + 5)).await.unwrap();
    assert_eq!(fx.fake.snap().open, 1);
    writer.abort().await.unwrap();
    let state = fx.fake.snap();
    assert_eq!(state.open, 0);
    assert_eq!(state.aborted, 1);
    assert!(fx.store.head(&key("aborted")).await.unwrap().is_none());
}

#[tokio::test]
async fn abort_before_any_part_makes_no_remote_call() {
    let fx = fixture();
    let mut writer = fx.store.begin(&key("early"), None).await.unwrap();
    writer.write(pattern(10)).await.unwrap();
    writer.abort().await.unwrap();
    let state = fx.fake.snap();
    assert_eq!(state.created, 0);
    assert_eq!(state.aborted, 0);
}

#[tokio::test]
async fn must_not_leave_an_upload_behind_after_drop() {
    let fx = fixture();
    let mut writer = fx.store.begin(&key("dropped"), None).await.unwrap();
    writer.write(pattern(PART * 2 + 5)).await.unwrap();
    drop(writer);
    fx.store.shutdown().await;
    let state = fx.fake.snap();
    assert_eq!(
        state.open, 0,
        "dropped writer must abort its multipart upload"
    );
    assert_eq!(state.aborted, 1);
    assert!(fx.store.head(&key("dropped")).await.unwrap().is_none());
}

#[tokio::test]
async fn must_not_leave_an_upload_behind_when_the_writing_task_is_cancelled() {
    let fx = fixture();
    let store = fx.store.clone();
    let task = tokio::spawn(async move {
        let mut writer = store.begin(&key("cancelled"), None).await.unwrap();
        loop {
            writer.write(pattern(PART)).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    while fx.fake.snap().created == 0 {
        tokio::task::yield_now().await;
    }
    task.abort();
    let _ = task.await;
    fx.store.shutdown().await;
    assert_eq!(fx.fake.snap().open, 0);
    assert!(fx.store.head(&key("cancelled")).await.unwrap().is_none());
}

#[tokio::test]
async fn failed_part_poisons_the_writer_and_drop_aborts() {
    let fx = fixture();
    fx.fake.state().fail_part_at = Some(1);
    let mut writer = fx.store.begin(&key("failing"), None).await.unwrap();
    let outcome = writer.write(pattern(PART * 3)).await;
    assert!(
        matches!(outcome, Err(BlobError::Unavailable(_))),
        "{outcome:?}"
    );
    assert!(matches!(
        writer.write(pattern(1)).await,
        Err(BlobError::WriterClosed)
    ));
    assert!(matches!(
        writer.commit().await,
        Err(BlobError::WriterClosed)
    ));
    fx.store.shutdown().await;
    assert_eq!(fx.fake.snap().open, 0);
    assert!(fx.store.head(&key("failing")).await.unwrap().is_none());
}

#[tokio::test]
async fn must_not_leave_an_upload_behind_when_complete_fails() {
    let fx = fixture();
    fx.fake.state().fail_complete = true;
    let mut writer = fx.store.begin(&key("nocomplete"), None).await.unwrap();
    writer.write(pattern(PART * 2 + 1)).await.unwrap();
    assert!(writer.commit().await.is_err());
    let state = fx.fake.snap();
    assert_eq!(state.open, 0);
    assert_eq!(state.aborted, 1);
    assert!(fx.store.head(&key("nocomplete")).await.unwrap().is_none());
}

#[tokio::test]
async fn abort_failure_is_reported_by_abort_and_swallowed_after_failed_commit() {
    let fx = fixture();
    fx.fake.state().fail_abort = true;
    let mut writer = fx.store.begin(&key("stuck"), None).await.unwrap();
    writer.write(pattern(PART + 1)).await.unwrap();
    assert!(writer.abort().await.is_err());

    fx.fake.state().fail_complete = true;
    let mut writer = fx.store.begin(&key("stuck2"), None).await.unwrap();
    writer.write(pattern(PART + 1)).await.unwrap();
    assert!(matches!(
        writer.commit().await,
        Err(BlobError::Unavailable(_))
    ));
}

#[tokio::test]
async fn upload_slot_is_released_at_commit() {
    let fx = fixture_with("", settings(1));
    let first = fx.store.begin(&key("one"), None).await.unwrap();
    assert_eq!(fx.store.available_upload_slots(), 0);
    first.commit().await.unwrap();
    assert_eq!(fx.store.available_upload_slots(), 1);
    fx.store
        .begin(&key("two"), None)
        .await
        .unwrap()
        .commit()
        .await
        .unwrap();
}

#[tokio::test]
async fn upload_slot_is_released_at_abort() {
    let fx = fixture_with("", settings(1));
    let first = fx.store.begin(&key("one"), None).await.unwrap();
    first.abort().await.unwrap();
    assert_eq!(fx.store.available_upload_slots(), 1);
}

#[tokio::test]
async fn upload_slot_is_released_at_drop() {
    let fx = fixture_with("", settings(1));
    let first = fx.store.begin(&key("one"), None).await.unwrap();
    drop(first);
    assert_eq!(fx.store.available_upload_slots(), 1);
}

#[tokio::test]
async fn must_not_start_more_uploads_than_the_limit_and_must_not_queue_forever() {
    let fx = fixture_with("", settings(2));
    let a = fx.store.begin(&key("a"), None).await.unwrap();
    let b = fx.store.begin(&key("b"), None).await.unwrap();
    let started = std::time::Instant::now();
    let third = fx.store.begin(&key("c"), None).await;
    // Slot exhaustion is backpressure: 503 with a retry hint, not "the store is down".
    assert!(
        matches!(
            third.err(),
            Some(BlobError::Overloaded {
                retry_after: Some(_)
            })
        ),
        "the third upload must be refused with a retry hint"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "begin must give up quickly"
    );
    drop((a, b));
    assert!(fx.store.begin(&key("c"), None).await.is_ok());
}

#[tokio::test]
async fn waiting_begin_gets_the_slot_when_one_is_freed() {
    let mut cfg = settings(1);
    cfg.permit_timeout = Duration::from_secs(5);
    let fx = fixture_with("", cfg);
    let first = fx.store.begin(&key("a"), None).await.unwrap();
    let store = fx.store.clone();
    let waiter = tokio::spawn(async move { store.begin(&key("b"), None).await.map(|_| ()) });
    tokio::task::yield_now().await;
    first.abort().await.unwrap();
    waiter.await.unwrap().unwrap();
}

#[tokio::test]
async fn must_not_buffer_more_than_one_part_per_upload() {
    let fx = fixture();
    let permit = Arc::clone(&fx.store.backend.uploads)
        .acquire_owned()
        .await
        .unwrap();
    let mut sink = S3Sink::new(
        Arc::clone(&fx.store.backend),
        ObjectPath::from("mem/test"),
        PART,
        None,
        permit,
    );
    let chunk = vec![7_u8; 333];
    for _ in 0..50 {
        sink.write(&chunk).await.unwrap();
        assert!(
            sink.buffer_capacity() <= PART,
            "buffer grew to {}",
            sink.buffer_capacity()
        );
    }
    sink.discard().await.unwrap();
}

#[tokio::test]
async fn big_chunks_never_make_the_buffer_exceed_the_part_size() {
    let fx = fixture();
    let permit = Arc::clone(&fx.store.backend.uploads)
        .acquire_owned()
        .await
        .unwrap();
    let mut sink = S3Sink::new(
        Arc::clone(&fx.store.backend),
        ObjectPath::from("mem/big"),
        PART,
        Some(10 * PART as u64),
        permit,
    );
    sink.write(&vec![1_u8; PART * 7 + 3]).await.unwrap();
    assert!(sink.buffer_capacity() <= PART);
    sink.discard().await.unwrap();
}

#[tokio::test]
async fn known_large_uploads_allocate_one_part_once_and_then_stay_bounded() {
    let fx = fixture();
    let permit = Arc::clone(&fx.store.backend.uploads)
        .acquire_owned()
        .await
        .unwrap();
    let mut sink = S3Sink::new(
        Arc::clone(&fx.store.backend),
        ObjectPath::from("mem/prealloc"),
        PART,
        Some(10 * PART as u64),
        permit,
    );
    sink.write(&[1_u8; 100]).await.unwrap();
    assert_eq!(
        sink.buffer_capacity(),
        PART,
        "announced size lets the first part allocate once"
    );
    sink.write(&vec![2_u8; PART + 50]).await.unwrap();
    assert!(sink.buffer_capacity() <= PART);
    sink.discard().await.unwrap();
}

#[tokio::test]
async fn unknown_size_does_not_reserve_a_whole_part_for_a_tiny_object() {
    let fx = fixture();
    let permit = Arc::clone(&fx.store.backend.uploads)
        .acquire_owned()
        .await
        .unwrap();
    let mut sink = S3Sink::new(
        Arc::clone(&fx.store.backend),
        ObjectPath::from("mem/tiny"),
        PART,
        None,
        permit,
    );
    sink.write(&[1_u8; 10]).await.unwrap();
    assert!(sink.buffer_capacity() < PART);
    sink.discard().await.unwrap();
}

#[tokio::test]
async fn must_not_exceed_the_part_limit() {
    let mut cfg = settings(1);
    cfg.part_size = 1;
    let fx = fixture_with("", cfg);
    let mut writer = fx.store.begin(&key("toomany"), None).await.unwrap();
    let outcome = writer.write(Bytes::from(vec![0_u8; MAX_PARTS + 2])).await;
    assert!(matches!(
        outcome,
        Err(BlobError::Backend {
            context: "upload part",
            ..
        })
    ));
    drop(writer);
    fx.store.shutdown().await;
    assert_eq!(fx.fake.snap().open, 0);
}

#[test]
fn part_size_grows_only_when_ten_thousand_parts_are_not_enough() {
    let fx = fixture_with(
        "",
        S3Settings {
            part_size: 16 << 20,
            ..settings(1)
        },
    );
    assert_eq!(fx.store.part_size_for(None), 16 << 20);
    assert_eq!(fx.store.part_size_for(Some(5 << 30)), 16 << 20);
    let huge = (16_u64 << 20) * MAX_PARTS as u64 * 2;
    assert_eq!(
        fx.store.part_size_for(Some(huge)) as u64,
        huge / MAX_PARTS as u64
    );
}

#[tokio::test]
async fn ranged_reads_follow_http_semantics() {
    let fx = fixture();
    let data = pattern(500);
    put(&fx, "ranged", &data, 100).await;
    let r = |start, end| Some(ByteRange { start, end });
    assert_eq!(
        read_all(&fx.store, "ranged", r(10, Some(19))).await,
        data[10..20].to_vec()
    );
    assert_eq!(
        read_all(&fx.store, "ranged", r(490, None)).await,
        data[490..].to_vec()
    );
    assert_eq!(
        read_all(&fx.store, "ranged", r(495, Some(9999))).await,
        data[495..].to_vec()
    );
    let read = fx.store.get(&key("ranged"), r(10, Some(19))).await.unwrap();
    assert_eq!(
        read.total_size, 500,
        "total_size is the object size, not the range size"
    );
    for bad in [r(500, None), r(600, Some(700)), r(20, Some(10))] {
        assert!(matches!(
            fx.store.get(&key("ranged"), bad).await,
            Err(BlobError::RangeNotSatisfiable)
        ));
    }
}

#[tokio::test]
async fn ranged_read_of_a_missing_object_is_not_found() {
    let fx = fixture();
    let range = Some(ByteRange {
        start: 0,
        end: Some(1),
    });
    assert!(matches!(
        fx.store.get(&key("nope"), range).await,
        Err(BlobError::NotFound)
    ));
    assert!(matches!(
        fx.store.get(&key("nope"), None).await,
        Err(BlobError::NotFound)
    ));
}

#[tokio::test]
async fn read_chunks_are_at_most_64_kib() {
    let fx = fixture();
    let data = pattern(300_000);
    put(&fx, "wide", &data, 50_000).await;
    let read = fx.store.get(&key("wide"), None).await.unwrap();
    let chunks: Vec<Bytes> = read.stream.try_collect().await.unwrap();
    assert!(chunks.iter().all(|c| c.len() <= READ_CHUNK_BYTES));
    assert_eq!(chunks.concat(), data.to_vec());
}

#[tokio::test]
async fn head_delete_and_missing_keys() {
    let fx = fixture();
    assert!(fx.store.head(&key("gone")).await.unwrap().is_none());
    fx.store.delete(&key("gone")).await.unwrap();
    put(&fx, "here", &pattern(10), 10).await;
    assert_eq!(
        fx.store
            .head(&key("here"))
            .await
            .unwrap()
            .unwrap()
            .size_bytes,
        10
    );
    fx.store.delete(&key("here")).await.unwrap();
    assert!(fx.store.head(&key("here")).await.unwrap().is_none());
}

#[tokio::test]
async fn objects_land_under_the_prefix_and_shard_directories() {
    let fx = fixture_with("tenant/blobs", settings(1));
    put(&fx, "abc", &pattern(5), 5).await;
    let listed: Vec<_> = fx.target.list(None).try_collect().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].location.as_ref(), "tenant/blobs/ba/78/abc");
}

#[tokio::test]
async fn must_not_escape_the_prefix_with_hostile_keys() {
    let fx = fixture_with("tenant", settings(1));
    for bad in [
        "",
        "../x",
        "/abs",
        "a//b",
        "a/../b",
        "a\0b",
        "_staging/x",
        "a b",
    ] {
        assert!(
            fx.store.begin(&key(bad), None).await.is_err(),
            "begin {bad:?}"
        );
        assert!(fx.store.get(&key(bad), None).await.is_err(), "get {bad:?}");
        assert!(fx.store.head(&key(bad)).await.is_err(), "head {bad:?}");
        assert!(fx.store.delete(&key(bad)).await.is_err(), "delete {bad:?}");
    }
    assert_empty(&fx.target.list(None).try_collect::<Vec<_>>().await.unwrap());
}

#[tokio::test]
async fn health_succeeds_when_the_probe_key_is_absent() {
    let fx = fixture();
    fx.store.health().await.unwrap();
}

#[tokio::test]
async fn stale_listing_is_empty_because_s3_cannot_list_uploads() {
    let fx = fixture();
    let mut writer = fx.store.begin(&key("open"), None).await.unwrap();
    writer.write(pattern(PART * 2)).await.unwrap();
    assert_empty(&fx.store.list_stale_staging(Duration::ZERO).await.unwrap());
    writer.abort().await.unwrap();
    assert_eq!(fx.store.kind(), BlobStoreKind::S3);
}

#[test]
fn store_errors_map_onto_the_port_contract() {
    let not_found = object_store::Error::NotFound {
        path: "p".into(),
        source: "x".into(),
    };
    assert!(matches!(store_error("get", not_found), BlobError::NotFound));
    assert!(matches!(
        store_error("get", injected("boom")),
        BlobError::Unavailable(_)
    ));
    let denied = object_store::Error::PermissionDenied {
        path: "p".into(),
        source: "x".into(),
    };
    assert!(matches!(
        store_error("get", denied),
        BlobError::Backend { context: "get", .. }
    ));
}

#[test]
fn from_backends_rejects_bad_prefix_and_settings() {
    let target = Arc::new(InMemory::new());
    let fake = Arc::new(FakeMultipart {
        target: Arc::clone(&target),
        state: Mutex::default(),
    });
    let build = |prefix: &str, settings: S3Settings| {
        S3BlobStore::from_backends(
            Arc::clone(&target) as Arc<dyn ObjectStore>,
            Arc::clone(&fake) as Arc<dyn MultipartStore>,
            prefix,
            settings,
        )
    };
    assert!(build("../x", settings(1)).is_err());
    assert!(build("a/", settings(1)).is_err());
    assert!(
        build(
            "",
            S3Settings {
                part_size: 0,
                ..settings(1)
            }
        )
        .is_err()
    );
    assert!(build("ok/prefix", settings(1)).is_ok());
}

#[test]
fn building_from_config_needs_no_network_and_validates() {
    let config: S3Config = crate::config::parse_config(
        "s3",
        &serde_json::json!({
            "bucket": "artifacts",
            "endpoint": "http://127.0.0.1:9000",
            "allow_http": true,
            "prefix": "prod",
        }),
    )
    .unwrap();
    let creds = S3Credentials {
        access_key_id: Secret::new("id"),
        secret_access_key: Secret::new("key"),
        session_token: Some(Secret::new("token")),
    };
    let store = S3BlobStore::new("unit", &config, Some(&creds)).unwrap();
    assert_eq!(store.kind(), BlobStoreKind::S3);
    let debug = format!("{store:?} {creds:?}");
    assert!(
        !debug.contains("\"key\"") && !debug.contains("token\""),
        "{debug}"
    );

    let mut bad = config.clone();
    bad.endpoint = Some("http://127.0.0.1:9000".into());
    bad.allow_http = false;
    assert!(S3BlobStore::new("unit", &bad, None).is_err());
}
