//! Behaviour of `LocalBlobStore` through the public port.
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use common::{byte_at, pattern};
use futures::TryStreamExt;
use rustryx_blob::{BlobHasher, LocalBlobStore, READ_CHUNK_BYTES, STAGING_DIR, shard_of};
use rustryx_core::BlobError;
use rustryx_core::ports::{BlobCommit, BlobKey, BlobStore, ByteRange};

async fn open() -> (tempfile::TempDir, LocalBlobStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalBlobStore::open(dir.path().join("blobs"))
        .await
        .unwrap();
    (dir, store)
}

fn key(text: &str) -> BlobKey {
    BlobKey::new(text)
}

fn digest(data: &[u8]) -> BlobCommit {
    let mut hasher = BlobHasher::new();
    hasher.update(data);
    hasher.finish()
}

async fn put(store: &LocalBlobStore, name: &str, data: &Bytes, chunk: usize) -> BlobCommit {
    let mut writer = store
        .begin(&key(name), Some(data.len() as u64))
        .await
        .unwrap();
    for piece in data.chunks(chunk.max(1)) {
        writer.write(data.slice_ref(piece)).await.unwrap();
    }
    writer.commit().await.unwrap()
}

async fn read_all(store: &LocalBlobStore, name: &str, range: Option<ByteRange>) -> Vec<u8> {
    let read = store.get(&key(name), range).await.unwrap();
    let chunks: Vec<Bytes> = read.stream.try_collect().await.unwrap();
    chunks.concat()
}

/// Every regular file under `dir`, found with async directory reads only.
async fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&next).await else {
            continue;
        };
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let meta = entry.metadata().await.unwrap();
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

async fn staged(store: &LocalBlobStore) -> Vec<PathBuf> {
    files_under(store.staging_dir()).await
}

/// Committed blobs: everything outside the staging directory.
async fn committed(store: &LocalBlobStore) -> Vec<PathBuf> {
    let all = files_under(store.root()).await;
    all.into_iter()
        .filter(|path| !path.starts_with(store.staging_dir()))
        .collect()
}

#[tokio::test]
async fn round_trip_returns_digests_size_and_content() {
    let (_dir, store) = open().await;
    let data = pattern(0, 200_003);
    let commit = put(&store, "0192f3a4-7b1c-7d2e-8f00-123456789abc", &data, 7_000).await;
    assert_eq!(commit, digest(&data));
    assert_eq!(commit.size_bytes, 200_003);
    let name = "0192f3a4-7b1c-7d2e-8f00-123456789abc";
    assert_eq!(read_all(&store, name, None).await, data.to_vec());
    assert_eq!(
        store.head(&key(name)).await.unwrap().unwrap().size_bytes,
        200_003
    );
    let read = store.get(&key(name), None).await.unwrap();
    assert_eq!(read.total_size, 200_003);
}

#[tokio::test]
async fn blobs_land_at_the_sharded_location() {
    let (_dir, store) = open().await;
    put(&store, "abc", &pattern(0, 10), 10).await;
    let (first, second) = shard_of("abc");
    let expected = store.root().join(first).join(second).join("abc");
    assert_eq!(committed(&store).await, vec![expected]);
}

#[tokio::test]
async fn nested_keys_round_trip_and_delete() {
    let (_dir, store) = open().await;
    let name = "_uploads/docker/0192f3a4-7b1c";
    let data = pattern(0, 4096);
    put(&store, name, &data, 1000).await;
    assert_eq!(read_all(&store, name, None).await, data.to_vec());
    store.delete(&key(name)).await.unwrap();
    assert!(store.head(&key(name)).await.unwrap().is_none());
}

#[tokio::test]
async fn empty_blob_round_trips() {
    let (_dir, store) = open().await;
    let commit = put(&store, "empty", &Bytes::new(), 1).await;
    assert_eq!(commit, digest(b""));
    assert_eq!(
        store.head(&key("empty")).await.unwrap().unwrap().size_bytes,
        0
    );
    assert_eq!(read_all(&store, "empty", None).await.len(), 0);
}

#[tokio::test]
async fn must_not_make_the_blob_visible_before_commit() {
    let (_dir, store) = open().await;
    let mut writer = store.begin(&key("pending"), None).await.unwrap();
    writer.write(pattern(0, 100_000)).await.unwrap();

    assert!(store.head(&key("pending")).await.unwrap().is_none());
    assert!(matches!(
        store.get(&key("pending"), None).await,
        Err(BlobError::NotFound)
    ));
    assert_eq!(committed(&store).await, Vec::<PathBuf>::new());
    assert_eq!(staged(&store).await.len(), 1);

    writer.commit().await.unwrap();
    assert_eq!(
        store
            .head(&key("pending"))
            .await
            .unwrap()
            .unwrap()
            .size_bytes,
        100_000
    );
    assert_eq!(staged(&store).await, Vec::<PathBuf>::new());
}

#[tokio::test]
async fn must_not_leave_a_partial_blob_after_abort() {
    let (_dir, store) = open().await;
    let mut writer = store.begin(&key("aborted"), None).await.unwrap();
    writer.write(pattern(0, 100_000)).await.unwrap();
    writer.abort().await.unwrap();
    assert_eq!(staged(&store).await, Vec::<PathBuf>::new());
    assert_eq!(committed(&store).await, Vec::<PathBuf>::new());
    assert!(store.head(&key("aborted")).await.unwrap().is_none());
}

#[tokio::test]
async fn must_not_leave_a_partial_blob_after_drop() {
    let (_dir, store) = open().await;
    let mut writer = store.begin(&key("dropped"), None).await.unwrap();
    writer.write(pattern(0, 100_000)).await.unwrap();
    drop(writer);
    store.drain_cleanup().await;
    assert_eq!(staged(&store).await, Vec::<PathBuf>::new());
    assert_eq!(committed(&store).await, Vec::<PathBuf>::new());
}

#[tokio::test]
async fn must_not_leave_a_partial_blob_when_the_writing_task_is_cancelled() {
    let (_dir, store) = open().await;
    let task_store = store.clone();
    let writing = tokio::spawn(async move {
        let mut writer = task_store.begin(&key("cancelled"), None).await.unwrap();
        loop {
            writer.write(pattern(0, 64 * 1024)).await.unwrap();
        }
    });
    while staged(&store).await.is_empty() {
        tokio::task::yield_now().await;
    }
    writing.abort();
    assert!(writing.await.unwrap_err().is_cancelled());
    store.drain_cleanup().await;
    assert_eq!(staged(&store).await, Vec::<PathBuf>::new());
    assert!(store.head(&key("cancelled")).await.unwrap().is_none());
}

#[tokio::test]
async fn must_not_leave_a_partial_blob_when_commit_fails() {
    let (_dir, store) = open().await;
    // A directory squatting on the final path makes the rename fail.
    let (first, second) = shard_of("blocked");
    let squatter = store.root().join(first).join(second).join("blocked");
    tokio::fs::create_dir_all(&squatter).await.unwrap();

    let mut writer = store.begin(&key("blocked"), None).await.unwrap();
    writer.write(pattern(0, 1000)).await.unwrap();
    assert!(matches!(
        writer.commit().await,
        Err(BlobError::Backend { .. })
    ));

    assert_eq!(staged(&store).await, Vec::<PathBuf>::new());
    assert_eq!(committed(&store).await, Vec::<PathBuf>::new());
    assert!(store.head(&key("blocked")).await.unwrap().is_none());
}

#[tokio::test]
async fn commit_replaces_an_existing_blob_atomically() {
    let (_dir, store) = open().await;
    put(&store, "same", &pattern(0, 5000), 5000).await;
    let second = pattern(7, 9000);
    put(&store, "same", &second, 9000).await;
    assert_eq!(read_all(&store, "same", None).await, second.to_vec());
    assert_eq!(committed(&store).await.len(), 1);
}

#[tokio::test]
async fn must_not_escape_the_root_with_hostile_keys() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside.txt");
    tokio::fs::write(&outside, b"precious").await.unwrap();
    let store = LocalBlobStore::open(dir.path().join("blobs"))
        .await
        .unwrap();
    let before = files_under(dir.path()).await;

    let hostile = [
        "",
        "..",
        "../outside.txt",
        "../../outside.txt",
        "a/../../outside.txt",
        "/etc/passwd",
        "/outside.txt",
        "a/./b",
        "a//b",
        "trailing/",
        "nul\0byte",
        "back\\slash",
        "..\\outside.txt",
        "white space",
        "_staging",
        "_staging/../../outside.txt",
        "_staging/not-a-staging-name.part",
        "caf\u{e9}",
    ];
    for bad in hostile {
        let k = key(bad);
        assert!(
            store.begin(&k, None).await.is_err(),
            "begin accepted {bad:?}"
        );
        assert!(store.get(&k, None).await.is_err(), "get accepted {bad:?}");
        assert!(store.head(&k).await.is_err(), "head accepted {bad:?}");
        assert!(store.delete(&k).await.is_err(), "delete accepted {bad:?}");
    }

    assert_eq!(tokio::fs::read(&outside).await.unwrap(), b"precious");
    assert_eq!(
        files_under(dir.path()).await,
        before,
        "no file may appear or vanish"
    );
}

#[tokio::test]
async fn invalid_key_error_carries_a_typed_source() {
    let (_dir, store) = open().await;
    let Err(BlobError::Backend { context, source }) = store.head(&key("../x")).await else {
        panic!("expected a backend error");
    };
    assert_eq!(context, "invalid blob key");
    assert!(source.downcast_ref::<rustryx_blob::KeyError>().is_some());
}

#[tokio::test]
async fn must_not_serve_staging_content_by_key() {
    let (_dir, store) = open().await;
    let mut writer = store.begin(&key("secret-in-flight"), None).await.unwrap();
    writer
        .write(Bytes::from_static(b"half an upload"))
        .await
        .unwrap();

    let stale = store.list_stale_staging(Duration::ZERO).await.unwrap();
    assert_eq!(stale.len(), 1);
    assert!(stale[0].as_str().starts_with("_staging/"));
    assert!(store.get(&stale[0], None).await.is_err());
    assert!(store.head(&stale[0]).await.is_err());
    writer.abort().await.unwrap();
}

#[tokio::test]
async fn stale_staging_is_listed_by_age_and_deletable() {
    let (_dir, store) = open().await;
    let mut writer = store.begin(&key("abandoned"), None).await.unwrap();
    writer.write(Bytes::from_static(b"x")).await.unwrap();
    // Noise that must not be reported: a foreign file and a committed blob.
    tokio::fs::write(store.staging_dir().join("notes.txt"), b"hi")
        .await
        .unwrap();
    put(&store, "done", &pattern(0, 10), 10).await;

    assert_eq!(
        store
            .list_stale_staging(Duration::from_secs(3600))
            .await
            .unwrap(),
        Vec::new()
    );
    let stale = store.list_stale_staging(Duration::ZERO).await.unwrap();
    assert_eq!(stale.len(), 1);

    store.delete(&stale[0]).await.unwrap();
    store.delete(&stale[0]).await.unwrap(); // deleting twice is fine
    assert_eq!(
        store.list_stale_staging(Duration::ZERO).await.unwrap(),
        Vec::new()
    );

    // The writer lost its temp file: its commit fails and publishes nothing.
    assert!(writer.commit().await.is_err());
    assert!(store.head(&key("abandoned")).await.unwrap().is_none());
    assert!(store.head(&key("done")).await.unwrap().is_some());
}

#[tokio::test]
async fn delete_of_a_missing_key_succeeds() {
    let (_dir, store) = open().await;
    store.delete(&key("never-existed")).await.unwrap();
    put(&store, "present", &pattern(0, 10), 10).await;
    store.delete(&key("present")).await.unwrap();
    store.delete(&key("present")).await.unwrap();
    assert!(store.head(&key("present")).await.unwrap().is_none());
}

#[tokio::test]
async fn missing_blob_reads_as_not_found() {
    let (_dir, store) = open().await;
    assert!(matches!(
        store.get(&key("nope"), None).await,
        Err(BlobError::NotFound)
    ));
    assert!(store.head(&key("nope")).await.unwrap().is_none());
}

#[tokio::test]
async fn key_below_a_regular_file_reads_as_missing() {
    let (_dir, store) = open().await;
    // Make the parent component of `file/child` a regular file, which is what a
    // shard collision between `file` and `file/child` would produce.
    let (first, second) = shard_of("file/child");
    let shard_dir = store.root().join(first).join(second);
    tokio::fs::create_dir_all(&shard_dir).await.unwrap();
    tokio::fs::write(shard_dir.join("file"), b"x")
        .await
        .unwrap();

    assert!(store.head(&key("file/child")).await.unwrap().is_none());
    assert!(matches!(
        store.get(&key("file/child"), None).await,
        Err(BlobError::NotFound)
    ));
    store.delete(&key("file/child")).await.unwrap();
}

#[tokio::test]
async fn ranged_reads_follow_http_semantics() {
    let (_dir, store) = open().await;
    let data = pattern(0, 150_000);
    put(&store, "ranged", &data, 16_000).await;
    let range = |start, end| Some(ByteRange { start, end });

    assert_eq!(
        read_all(&store, "ranged", range(0, Some(0))).await,
        data[..1].to_vec()
    );
    assert_eq!(
        read_all(&store, "ranged", range(10, Some(70_009))).await,
        data[10..70_010].to_vec()
    );
    assert_eq!(
        read_all(&store, "ranged", range(149_990, None)).await,
        data[149_990..].to_vec()
    );
    assert_eq!(
        read_all(&store, "ranged", range(149_999, Some(10_000_000))).await,
        data[149_999..].to_vec()
    );
    let read = store.get(&key("ranged"), range(5, Some(9))).await.unwrap();
    assert_eq!(
        read.total_size, 150_000,
        "total_size is the object size, not the range size"
    );

    for bad in [
        range(150_000, None),
        range(200_000, Some(300_000)),
        range(9, Some(5)),
    ] {
        assert!(matches!(
            store.get(&key("ranged"), bad).await,
            Err(BlobError::RangeNotSatisfiable)
        ));
    }
    put(&store, "zero", &Bytes::new(), 1).await;
    assert!(matches!(
        store.get(&key("zero"), range(0, None)).await,
        Err(BlobError::RangeNotSatisfiable)
    ));
}

#[tokio::test]
async fn read_chunks_are_bounded_and_complete() {
    let (_dir, store) = open().await;
    let data = pattern(0, 1_000_003);
    put(&store, "chunks", &data, 100_000).await;
    let read = store.get(&key("chunks"), None).await.unwrap();
    let chunks: Vec<Bytes> = read.stream.try_collect().await.unwrap();
    assert!(
        chunks
            .iter()
            .all(|c| c.len() <= READ_CHUNK_BYTES && !c.is_empty())
    );
    assert!(chunks.len() >= 1_000_003 / READ_CHUNK_BYTES);
    assert_eq!(chunks.concat(), data.to_vec());
}

#[tokio::test]
async fn read_stream_is_pull_based() {
    let (_dir, store) = open().await;
    put(&store, "lazy", &pattern(0, 300_000), 50_000).await;
    let mut read = store.get(&key("lazy"), None).await.unwrap();
    let first = futures::StreamExt::next(&mut read.stream)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first[0], byte_at(0));
    // Dropping a half-read stream releases the file; the blob stays intact.
    drop(read);
    assert_eq!(
        store.head(&key("lazy")).await.unwrap().unwrap().size_bytes,
        300_000
    );
}

#[tokio::test]
async fn commit_is_atomic_under_concurrent_writers_to_different_keys() {
    let (_dir, store) = open().await;
    let writers = 24_usize;
    let mut tasks = Vec::new();
    for i in 0..writers {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            let data = pattern(i as u64 * 13, 20_000 + i * 4_321);
            let commit = put(&store, &format!("blob-{i}"), &data, 3_000 + i * 17).await;
            (i, data, commit)
        }));
    }

    // A reader races the writers: a key is either absent or complete and correct.
    let reader_store = store.clone();
    let reader = tokio::spawn(async move {
        let mut seen_complete = 0;
        for _ in 0..200 {
            for i in 0..writers {
                let name = format!("blob-{i}");
                let Some(meta) = reader_store.head(&key(&name)).await.unwrap() else {
                    continue;
                };
                assert_eq!(
                    meta.size_bytes,
                    (20_000 + i * 4_321) as u64,
                    "{name} visible half written"
                );
                let expected = pattern(i as u64 * 13, 20_000 + i * 4_321);
                assert_eq!(
                    read_all(&reader_store, &name, None).await,
                    expected.to_vec()
                );
                seen_complete += 1;
            }
            tokio::task::yield_now().await;
        }
        seen_complete
    });

    for task in tasks {
        let (i, data, commit) = task.await.unwrap();
        assert_eq!(commit, digest(&data), "digests of writer {i}");
        assert_eq!(
            read_all(&store, &format!("blob-{i}"), None).await,
            data.to_vec()
        );
    }
    reader.await.unwrap();
    assert_eq!(staged(&store).await, Vec::<PathBuf>::new());
    assert_eq!(committed(&store).await.len(), writers);
}

#[tokio::test]
async fn health_reports_a_writable_root_and_fails_without_it() {
    let (dir, store) = open().await;
    store.health().await.unwrap();
    assert_eq!(
        staged(&store).await,
        Vec::<PathBuf>::new(),
        "probe must clean up after itself"
    );

    tokio::fs::remove_dir_all(dir.path().join("blobs"))
        .await
        .unwrap();
    assert!(matches!(
        store.health().await,
        Err(BlobError::Unavailable(_))
    ));
}

#[tokio::test]
async fn begin_recreates_a_wiped_staging_directory() {
    let (_dir, store) = open().await;
    tokio::fs::remove_dir_all(store.staging_dir())
        .await
        .unwrap();
    put(&store, "revived", &pattern(0, 100), 100).await;
    assert!(store.head(&key("revived")).await.unwrap().is_some());
}

#[tokio::test]
async fn open_rejects_a_root_that_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a-file");
    tokio::fs::write(&file, b"x").await.unwrap();
    assert!(LocalBlobStore::open(&file).await.is_err());
}

#[tokio::test]
async fn store_reports_kind_root_and_debug() {
    use rustryx_core::model::BlobStoreKind;
    let (_dir, store) = open().await;
    assert_eq!(store.kind(), BlobStoreKind::Local);
    assert!(store.staging_dir().ends_with(STAGING_DIR));
    assert!(format!("{store:?}").contains("LocalBlobStore"));
}
