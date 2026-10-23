//! The free space floor of the local store (SRE-34): writes are refused below it, reads,
//! listings and deletes are not, and the floor follows the disk.
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::FakeDisk;
use futures::TryStreamExt;
use rustryx_blob::{DiskProbe, LocalBlobStore, LocalOptions, StatvfsProbe};
use rustryx_core::BlobError;
use rustryx_core::ports::{BlobKey, BlobStore};

/// A store over a fake 1000 byte disk with `free` bytes free and a floor of 10 percent.
async fn open(free: u64) -> (tempfile::TempDir, Arc<FakeDisk>, LocalBlobStore) {
    open_with_floor(free, 10).await
}

async fn open_with_floor(
    free: u64,
    percent: u8,
) -> (tempfile::TempDir, Arc<FakeDisk>, LocalBlobStore) {
    let dir = tempfile::tempdir().unwrap();
    let disk = FakeDisk::new(1000, free);
    let options = LocalOptions::new("disk-it")
        .with_disk_floor(Arc::clone(&disk) as Arc<dyn DiskProbe>, percent);
    let store = LocalBlobStore::open_with(dir.path(), options)
        .await
        .unwrap();
    (dir, disk, store)
}

fn key(name: &str) -> BlobKey {
    BlobKey::new(name)
}

async fn put(store: &LocalBlobStore, name: &str, len: usize) {
    let mut writer = store.begin(&key(name), Some(len as u64)).await.unwrap();
    writer.write(Bytes::from(vec![1_u8; len])).await.unwrap();
    writer.commit().await.unwrap();
}

async fn staging_is_empty(store: &LocalBlobStore) -> bool {
    let mut entries = tokio::fs::read_dir(store.staging_dir()).await.unwrap();
    entries.next_entry().await.unwrap().is_none()
}

#[tokio::test]
async fn must_not_accept_a_write_below_the_free_space_floor() {
    let (_dir, _disk, store) = open(99).await;
    let outcome = store.begin(&key("refused"), None).await;
    assert!(
        matches!(outcome.err(), Some(BlobError::InsufficientStorage)),
        "99 of 1000 bytes free is below 10 percent"
    );
    assert!(staging_is_empty(&store).await, "nothing was created");
    assert!(store.head(&key("refused")).await.unwrap().is_none());
}

#[tokio::test]
async fn a_write_may_leave_exactly_the_floor_but_not_less() {
    // The floor is what must remain AFTER the write: 101 free allows one more byte (100
    // left), 100 free does not.
    let (_dir, _disk, store) = open(101).await;
    put(&store, "leaves-the-floor", 1).await;

    let (_dir, _disk, store) = open(100).await;
    let mut writer = store.begin(&key("at-the-floor"), None).await.unwrap();
    assert!(matches!(
        writer.write(Bytes::from_static(b"x")).await,
        Err(BlobError::InsufficientStorage)
    ));
}

#[tokio::test]
async fn a_write_that_announces_a_size_across_the_floor_is_refused_before_any_byte() {
    let (_dir, _disk, store) = open(300).await;
    // 300 free; 200 more would leave exactly the floor, 201 would cross it.
    let ok = store.begin(&key("fits"), Some(200)).await.unwrap();
    ok.abort().await.unwrap();
    let refused = store.begin(&key("too-big"), Some(201)).await;
    assert!(matches!(
        refused.err(),
        Some(BlobError::InsufficientStorage)
    ));
}

#[tokio::test]
async fn an_upload_that_crosses_the_floor_midway_stops_and_leaves_nothing() {
    let (_dir, _disk, store) = open(300).await;
    let mut writer = store.begin(&key("grows"), None).await.unwrap();
    writer.write(Bytes::from(vec![0_u8; 150])).await.unwrap();
    // 300 - 150 = 150 left; another 60 leaves 90, below the floor of 100.
    let outcome = writer.write(Bytes::from(vec![0_u8; 60])).await;
    assert!(matches!(outcome, Err(BlobError::InsufficientStorage)));
    assert!(matches!(
        writer.commit().await,
        Err(BlobError::WriterClosed)
    ));
    assert!(staging_is_empty(&store).await);
    assert!(store.head(&key("grows")).await.unwrap().is_none());
}

#[tokio::test]
async fn bytes_written_since_the_last_sample_count_against_the_free_space() {
    // The sample says 300 free and the monitor will not look again for an hour, so the
    // guard has to account for what it has let through itself.
    let (_dir, _disk, store) = open(300).await;
    let mut first = store.begin(&key("one"), None).await.unwrap();
    first.write(Bytes::from(vec![0_u8; 100])).await.unwrap();
    let mut second = store.begin(&key("two"), None).await.unwrap();
    second.write(Bytes::from(vec![0_u8; 100])).await.unwrap();
    // 100 left on paper; one more byte from either writer crosses the floor.
    assert!(matches!(
        first.write(Bytes::from_static(b"x")).await,
        Err(BlobError::InsufficientStorage)
    ));
    assert!(matches!(
        second.write(Bytes::from_static(b"x")).await,
        Err(BlobError::InsufficientStorage)
    ));
}

#[tokio::test]
async fn must_not_block_reads_listings_or_deletes_when_the_disk_is_full() {
    let (_dir, disk, store) = open(1000).await;
    put(&store, "kept", 64).await;
    put(&store, "doomed", 32).await;

    disk.set_free(0);
    store.refresh_disk().await;
    assert!(matches!(
        store.begin(&key("new"), None).await.err(),
        Some(BlobError::InsufficientStorage)
    ));

    let read = store.get(&key("kept"), None).await.unwrap();
    let chunks: Vec<Bytes> = read.stream.try_collect().await.unwrap();
    assert_eq!(chunks.concat().len(), 64);
    assert_eq!(
        store.head(&key("kept")).await.unwrap().unwrap().size_bytes,
        64
    );
    assert_eq!(store.list_keys(None, 10).await.unwrap().keys.len(), 2);
    assert_eq!(
        store
            .list_stale_staging(Duration::ZERO)
            .await
            .unwrap()
            .len(),
        0
    );
    store.health().await.unwrap();
    // Deleting is how a full disk recovers.
    store.delete(&key("doomed")).await.unwrap();
    assert!(store.head(&key("doomed")).await.unwrap().is_none());
}

#[tokio::test]
async fn the_floor_follows_the_disk_in_both_directions() {
    let (_dir, disk, store) = open(1000).await;
    put(&store, "before", 1).await;

    disk.set_free(50);
    store.refresh_disk().await;
    assert!(store.begin(&key("during"), None).await.is_err());

    disk.set_free(900);
    store.refresh_disk().await;
    put(&store, "after", 1).await;
}

#[tokio::test]
async fn a_floor_of_zero_never_refuses_but_the_disk_is_still_monitored() {
    let (_dir, disk, store) = open_with_floor(0, 0).await;
    put(&store, "no-floor", 10).await;
    disk.set_free(0);
    store.refresh_disk().await;
    put(&store, "still-no-floor", 10).await;
}

#[tokio::test]
async fn a_disk_that_cannot_be_probed_does_not_stop_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let disk = FakeDisk::new(1000, 0);
    disk.fail(true);
    let options = LocalOptions::new("disk-blind")
        .with_disk_floor(Arc::clone(&disk) as Arc<dyn DiskProbe>, 10);
    let store = LocalBlobStore::open_with(dir.path(), options)
        .await
        .unwrap();
    // Nothing is known, so nothing is refused; the first good sample takes over.
    put(&store, "blind", 10).await;
    disk.fail(false);
    store.refresh_disk().await;
    assert!(store.begin(&key("seen"), None).await.is_err());
}

#[tokio::test]
async fn shutdown_stops_the_monitor_and_refresh_returns_at_once_afterwards() {
    let (_dir, disk, store) = open(1000).await;
    store.shutdown().await;
    store.shutdown().await;
    disk.set_free(0);
    let started = std::time::Instant::now();
    store.refresh_disk().await;
    assert!(started.elapsed() < Duration::from_secs(1));
    // The last sample stays in force: a store that is shutting down does not start
    // refusing or accepting differently.
    put(&store, "after-shutdown", 1).await;
}

#[tokio::test]
async fn the_real_probe_works_on_the_machine_running_the_tests() {
    let dir = tempfile::tempdir().unwrap();
    let options = LocalOptions::new("disk-real").with_disk_floor(Arc::new(StatvfsProbe), 0);
    let store = LocalBlobStore::open_with(dir.path(), options)
        .await
        .unwrap();
    put(&store, "real", 10).await;
    store.refresh_disk().await;
}

#[tokio::test]
async fn options_describe_themselves_without_the_probe_internals() {
    let options = LocalOptions::new("shown")
        .with_disk_floor(Arc::new(StatvfsProbe), 12)
        .with_probe_interval(Duration::from_secs(7));
    let text = format!("{options:?}");
    assert!(text.contains("shown") && text.contains("12") && text.contains("monitored: true"));
}
