//! REQ-01/REQ-02 for the local store: a very large blob streams through the store
//! without the process holding it in memory.
//!
//! This file must keep exactly one test. RSS is a per-process number, and the test
//! harness runs the tests of one binary in parallel threads of one process, so any
//! second test would pollute the measurement.
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use bytes::Bytes;
use common::rss_kib;
use futures::StreamExt;
use rustryx_blob::{BlobHasher, LocalBlobStore};
use rustryx_core::ports::{BlobKey, BlobStore};

const TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;
const MAX_GROWTH_KIB: u64 = 64 * 1024;
const SAMPLE_EVERY_CHUNKS: u64 = 128;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn must_not_buffer_whole_blob_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalBlobStore::open(dir.path()).await.unwrap();
    let key = BlobKey::new("0192f3a4-7b1c-7d2e-8f00-5ee0b10b0001");

    // One fixed chunk, cloned by reference count: the producer itself allocates
    // nothing per chunk, so every byte of growth belongs to the store.
    let chunk = Bytes::from(
        (0..CHUNK_BYTES)
            .map(|i| u8::try_from(i % 253).unwrap())
            .collect::<Vec<u8>>(),
    );
    let chunks = TOTAL_BYTES / CHUNK_BYTES as u64;

    // The meter itself must work: a 100 MiB allocation has to show up in RSS, otherwise
    // a broken `ps` (reporting 0) would make the assertion below pass vacuously.
    let before_ballast = rss_kib();
    let ballast = vec![1_u8; 100 * 1024 * 1024];
    let with_ballast = rss_kib();
    assert!(
        with_ballast >= before_ballast + 90 * 1024,
        "RSS meter did not see a 100 MiB allocation ({before_ballast} -> {with_ballast} KiB)"
    );
    drop(ballast);

    // Warm up thread pools and allocator arenas so they are not billed to the blob.
    let mut warm = store.begin(&BlobKey::new("warmup"), None).await.unwrap();
    for _ in 0..16 {
        warm.write(chunk.clone()).await.unwrap();
    }
    warm.commit().await.unwrap();
    store.delete(&BlobKey::new("warmup")).await.unwrap();

    let baseline = rss_kib();
    let mut peak = baseline;

    let mut writer = store.begin(&key, Some(TOTAL_BYTES)).await.unwrap();
    for index in 0..chunks {
        writer.write(chunk.clone()).await.unwrap();
        if index.is_multiple_of(SAMPLE_EVERY_CHUNKS) {
            peak = peak.max(rss_kib());
        }
    }
    let commit = writer.commit().await.unwrap();
    peak = peak.max(rss_kib());
    assert_eq!(commit.size_bytes, TOTAL_BYTES);

    let read = store.get(&key, None).await.unwrap();
    assert_eq!(read.total_size, TOTAL_BYTES);
    let mut stream = read.stream;
    let mut verify = BlobHasher::new();
    let mut seen = 0_u64;
    let mut index = 0_u64;
    while let Some(piece) = stream.next().await {
        let piece = piece.unwrap();
        verify.update(&piece);
        seen += piece.len() as u64;
        if index.is_multiple_of(SAMPLE_EVERY_CHUNKS * 2) {
            peak = peak.max(rss_kib());
        }
        index += 1;
    }
    assert_eq!(seen, TOTAL_BYTES);
    assert_eq!(
        verify.finish().checksums,
        commit.checksums,
        "read back differs from what was written"
    );

    let growth = peak.saturating_sub(baseline);
    assert!(
        growth < MAX_GROWTH_KIB,
        "RSS grew by {} MiB while streaming {} MiB (limit {} MiB)",
        growth / 1024,
        TOTAL_BYTES / (1024 * 1024),
        MAX_GROWTH_KIB / 1024
    );
    eprintln!(
        "streamed {} MiB, RSS growth {} KiB",
        TOTAL_BYTES / (1024 * 1024),
        growth
    );
}
