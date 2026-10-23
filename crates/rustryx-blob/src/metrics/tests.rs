use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::TryStreamExt;
use rustryx_core::BlobError;
use rustryx_core::ports::{BlobKey, BlobStore, ByteRange};

use super::capture::capture;
use super::*;
use crate::disk::testing::FakeDisk;
use crate::local::{LocalBlobStore, LocalOptions};

fn ops(store: &str, op: &str, result: &str) -> u64 {
    capture().counter(
        OPS_TOTAL,
        &[("store", store), ("op", op), ("result", result)],
    )
}

fn bytes_of(store: &str, direction: &str) -> u64 {
    capture().counter(BYTES_TOTAL, &[("store", store), ("direction", direction)])
}

async fn open(name: &str) -> (tempfile::TempDir, LocalBlobStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalBlobStore::open_with(dir.path(), LocalOptions::new(name))
        .await
        .unwrap();
    (dir, store)
}

#[test]
fn every_error_variant_has_a_class() {
    let cases: Vec<(BlobError, Outcome)> = vec![
        (BlobError::NotFound, Outcome::NotFound),
        (BlobError::RangeNotSatisfiable, Outcome::Rejected),
        (BlobError::InvalidKey("x"), Outcome::Rejected),
        (BlobError::Unsupported("x"), Outcome::Rejected),
        (BlobError::WriterClosed, Outcome::Rejected),
        (
            BlobError::Overloaded { retry_after: None },
            Outcome::Overloaded,
        ),
        (BlobError::Unavailable("x".into()), Outcome::Error),
        (BlobError::InsufficientStorage, Outcome::Error),
    ];
    for (err, want) in cases {
        assert_eq!(outcome_of_error(&err), want, "{err:?}");
    }
    let backend = crate::error::backend("ctx", "boom");
    assert_eq!(outcome_of_error(&backend), Outcome::Error);
}

#[test]
fn labels_are_the_closed_sets_of_the_documentation() {
    let ops: BTreeSet<_> = [
        Op::Put,
        Op::Get,
        Op::Head,
        Op::Delete,
        Op::List,
        Op::Presign,
        Op::Health,
    ]
    .map(Op::label)
    .into();
    assert_eq!(ops.len(), 7);
    let results: BTreeSet<_> = [
        Outcome::Ok,
        Outcome::NotFound,
        Outcome::Rejected,
        Outcome::Overloaded,
        Outcome::Aborted,
        Outcome::Error,
    ]
    .map(Outcome::label)
    .into();
    assert_eq!(results.len(), 6);
    assert_eq!(Direction::Write.label(), "write");
    assert_eq!(Direction::Read.label(), "read");
}

#[tokio::test]
async fn a_full_lifecycle_counts_each_operation_once_with_the_documented_labels() {
    let (_dir, store) = open("metrics-lifecycle").await;
    let key = BlobKey::new("k-lifecycle");

    let mut writer = store.begin(&key, None).await.unwrap();
    writer.write(Bytes::from(vec![7_u8; 1000])).await.unwrap();
    writer.write(Bytes::from(vec![8_u8; 500])).await.unwrap();
    writer.commit().await.unwrap();
    assert_eq!(ops("metrics-lifecycle", "put", "ok"), 1);
    assert_eq!(bytes_of("metrics-lifecycle", "write"), 1500);

    store.head(&key).await.unwrap().unwrap();
    assert_eq!(ops("metrics-lifecycle", "head", "ok"), 1);

    let read = store.get(&key, None).await.unwrap();
    let _: Vec<Bytes> = read.stream.try_collect().await.unwrap();
    assert_eq!(ops("metrics-lifecycle", "get", "ok"), 1);
    assert_eq!(bytes_of("metrics-lifecycle", "read"), 1500);

    store.list_keys(None, 10).await.unwrap();
    store.list_stale_staging(Duration::ZERO).await.unwrap();
    assert_eq!(ops("metrics-lifecycle", "list", "ok"), 2);

    store.health().await.unwrap();
    assert_eq!(ops("metrics-lifecycle", "health", "ok"), 1);

    store.delete(&key).await.unwrap();
    assert_eq!(ops("metrics-lifecycle", "delete", "ok"), 1);
    assert_eq!(ops("metrics-lifecycle", "put", "error"), 0);
}

#[tokio::test]
async fn failures_are_split_into_the_classes_the_alert_distinguishes() {
    let (_dir, store) = open("metrics-failures").await;
    let missing = BlobKey::new("never-written");

    assert!(store.get(&missing, None).await.is_err());
    assert_eq!(ops("metrics-failures", "get", "not_found"), 1);

    let bad = BlobKey::new("../escape");
    assert!(store.head(&bad).await.is_err());
    assert!(store.begin(&bad, None).await.is_err());
    assert_eq!(ops("metrics-failures", "head", "rejected"), 1);
    assert_eq!(ops("metrics-failures", "put", "rejected"), 1);

    let range = Some(ByteRange {
        start: 10,
        end: None,
    });
    let key = BlobKey::new("short-one");
    let mut writer = store.begin(&key, None).await.unwrap();
    writer.write(Bytes::from_static(b"abc")).await.unwrap();
    writer.commit().await.unwrap();
    assert!(store.get(&key, range).await.is_err());
    assert_eq!(ops("metrics-failures", "get", "rejected"), 1);

    // Only `error` feeds the alert, and nothing above was an error of the store.
    for op in ["put", "get", "head", "delete", "list", "health"] {
        assert_eq!(ops("metrics-failures", op, "error"), 0, "{op}");
    }
}

#[tokio::test]
async fn an_aborted_write_is_counted_once_and_a_dropped_one_not_at_all() {
    let (_dir, store) = open("metrics-abort").await;
    let mut writer = store.begin(&BlobKey::new("a1"), None).await.unwrap();
    writer.write(Bytes::from_static(b"x")).await.unwrap();
    writer.abort().await.unwrap();
    assert_eq!(ops("metrics-abort", "put", "aborted"), 1);

    let mut writer = store.begin(&BlobKey::new("a2"), None).await.unwrap();
    writer.write(Bytes::from_static(b"x")).await.unwrap();
    drop(writer);
    store.shutdown().await;
    assert_eq!(ops("metrics-abort", "put", "aborted"), 1);
    assert_eq!(ops("metrics-abort", "put", "ok"), 0);
}

#[tokio::test]
async fn a_failed_write_counts_as_one_error_however_often_it_is_retried() {
    let disk = Arc::new(FakeDisk::new(1000, 1000));
    let dir = tempfile::tempdir().unwrap();
    let options = LocalOptions::new("metrics-error")
        .with_disk_floor(Arc::clone(&disk) as Arc<dyn crate::disk::DiskProbe>, 10);
    let store = LocalBlobStore::open_with(dir.path(), options)
        .await
        .unwrap();

    let mut writer = store.begin(&BlobKey::new("w"), None).await.unwrap();
    writer.write(Bytes::from(vec![0_u8; 100])).await.unwrap();
    // 100 of 1000 bytes were written, leaving 90%; another 850 would cross the floor.
    assert!(matches!(
        writer.write(Bytes::from(vec![0_u8; 850])).await,
        Err(BlobError::InsufficientStorage)
    ));
    assert!(matches!(
        writer.write(Bytes::from_static(b"again")).await,
        Err(BlobError::WriterClosed)
    ));
    assert!(matches!(
        writer.commit().await,
        Err(BlobError::WriterClosed)
    ));
    assert_eq!(ops("metrics-error", "put", "error"), 1);
    assert_eq!(ops("metrics-error", "put", "rejected"), 0);
}

#[tokio::test]
async fn the_disk_gauges_follow_the_probe() {
    let disk = Arc::new(FakeDisk::new(4096, 1024));
    let dir = tempfile::tempdir().unwrap();
    let options = LocalOptions::new("metrics-gauges")
        .with_disk_floor(Arc::clone(&disk) as Arc<dyn crate::disk::DiskProbe>, 0);
    let store = LocalBlobStore::open_with(dir.path(), options)
        .await
        .unwrap();
    let labels = [("store", "metrics-gauges")];
    assert_eq!(capture().gauge(FREE_BYTES, &labels), Some(1024.0));
    assert_eq!(capture().gauge(TOTAL_BYTES, &labels), Some(4096.0));

    disk.set(4096, 512);
    store.refresh_disk().await;
    assert_eq!(capture().gauge(FREE_BYTES, &labels), Some(512.0));
}

#[tokio::test]
async fn must_not_put_a_key_or_a_path_into_a_label() {
    let (dir, store) = open("metrics-cardinality").await;
    let key = BlobKey::new("distinctive-key-0192f3a4/nested/path.bin");
    let mut writer = store.begin(&key, None).await.unwrap();
    writer.write(Bytes::from_static(b"payload")).await.unwrap();
    writer.commit().await.unwrap();
    store.head(&key).await.unwrap();
    let read = store.get(&key, None).await.unwrap();
    let _: Vec<Bytes> = read.stream.try_collect().await.unwrap();
    store.delete(&key).await.unwrap();
    assert!(
        store
            .head(&BlobKey::new("../distinctive-escape"))
            .await
            .is_err()
    );

    let allowed: [(&str, &[&str]); 4] = [
        (OPS_TOTAL, &["store", "op", "result"]),
        (BYTES_TOTAL, &["store", "direction"]),
        (FREE_BYTES, &["store"]),
        (TOTAL_BYTES, &["store"]),
    ];
    let root = dir.path().display().to_string();
    for (name, label_names) in allowed {
        for labels in capture().label_sets(name) {
            let names: Vec<_> = labels.keys().map(String::as_str).collect();
            let mut expected = label_names.to_vec();
            expected.sort_unstable();
            assert_eq!(names, expected, "{name}");
            for value in labels.values() {
                assert!(!value.contains("distinctive"), "{name}: {value}");
                assert!(!value.contains(&root), "{name}: {value}");
            }
        }
    }
}
