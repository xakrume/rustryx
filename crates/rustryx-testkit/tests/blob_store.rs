//! `MemBlobStore` against the documented `BlobStore` contract.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    clippy::assert_is_empty,
    reason = "each test is one scenario read top to bottom; filters and non exhaustive types are \
              filled field by field after `default()`, and `.is_empty()` reads better than a \
              comparison with an empty literal of an inferred type"
)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use rustryx_core::BlobError;
use rustryx_core::model::BlobStoreKind;
use rustryx_core::ports::{BlobKey, BlobStore, ByteRange, PresignOptions, UploadHandle};
use rustryx_testkit::fixtures::{checksums_of, collect_stream};
use rustryx_testkit::{FixedClock, MemBlobStore};
use sha2::Digest;

fn key(name: &str) -> BlobKey {
    BlobKey::new(name)
}

async fn write_all(
    store: &MemBlobStore,
    name: &str,
    parts: &[&[u8]],
) -> rustryx_core::ports::BlobCommit {
    let mut writer = store.begin(&key(name), None).await.unwrap();
    for part in parts {
        writer.write(Bytes::copy_from_slice(part)).await.unwrap();
    }
    writer.commit().await.unwrap()
}

#[tokio::test]
async fn commit_reports_known_digests_for_abc() {
    let store = MemBlobStore::new();
    let commit = write_all(&store, "k", &[b"a", b"b", b"c"]).await;
    assert_eq!(commit.size_bytes, 3);
    assert_eq!(
        commit.checksums.sha1,
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        commit.checksums.sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(commit.checksums.md5, "900150983cd24fb0d6963f7d28e17f72");
}

#[tokio::test]
async fn commit_of_nothing_reports_the_empty_digests() {
    let store = MemBlobStore::new();
    let commit = write_all(&store, "empty", &[]).await;
    assert_eq!(commit.size_bytes, 0);
    assert_eq!(
        commit.checksums.sha1,
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    assert_eq!(
        commit.checksums.sha256,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(commit.checksums.md5, "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(
        store.head(&key("empty")).await.unwrap().unwrap().size_bytes,
        0
    );
}

/// One million `a` is the classic multi-block vector. The 100003 byte writes are larger
/// than the 64 KiB hashing slice and not a multiple of any block size, so slice and
/// chunk boundaries fall inside hash blocks.
#[tokio::test]
async fn streamed_digests_match_the_million_a_vectors() {
    let store = MemBlobStore::new();
    let body = vec![b'a'; 1_000_000];
    let parts: Vec<&[u8]> = body.chunks(100_003).collect();
    let commit = write_all(&store, "million", &parts).await;
    assert_eq!(commit.size_bytes, 1_000_000);
    assert_eq!(
        commit.checksums.sha1,
        "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
    );
    assert_eq!(
        commit.checksums.sha256,
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
    assert_eq!(commit.checksums.md5, "7707d6ae4e027c70eea2a935c2296f21");
}

#[tokio::test]
async fn chunking_of_writes_does_not_change_the_digests() {
    let body: Vec<u8> = (0..300_000_u32)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let store = MemBlobStore::new();
    let whole = write_all(&store, "whole", &[&body]).await;
    let tiny: Vec<&[u8]> = body.chunks(7).collect();
    let pieces = write_all(&store, "pieces", &tiny).await;
    assert_eq!(whole, pieces);
    assert_eq!(
        whole.checksums.sha256,
        hex::encode(sha2::Sha256::digest(&body))
    );
    assert_eq!(whole.checksums, checksums_of(&body));
}

#[tokio::test]
async fn must_not_expose_an_object_before_commit() {
    let store = MemBlobStore::new();
    let k = key("pending");
    let mut writer = store.begin(&k, Some(10)).await.unwrap();
    writer.write(Bytes::from_static(b"partial")).await.unwrap();
    assert!(store.head(&k).await.unwrap().is_none());
    assert!(matches!(
        store.get(&k, None).await,
        Err(BlobError::NotFound)
    ));
    assert_eq!(store.keys().len(), 0);
    assert_eq!(store.staging_count(), 1);
    writer.commit().await.unwrap();
    assert_eq!(store.staging_count(), 0);
    assert_eq!(store.head(&k).await.unwrap().unwrap().size_bytes, 7);
    assert!(store.contains(&k));
}

#[tokio::test]
async fn abort_discards_everything_written() {
    let store = MemBlobStore::new();
    let k = key("aborted");
    let mut writer = store.begin(&k, None).await.unwrap();
    writer.write(Bytes::from_static(b"data")).await.unwrap();
    writer.abort().await.unwrap();
    assert!(!store.contains(&k));
    assert_eq!(store.staging_count(), 0);
    assert!(store.is_empty());
}

#[tokio::test]
async fn dropped_writer_is_cleaned_up_by_default() {
    let store = MemBlobStore::new();
    let mut writer = store.begin(&key("dropped"), None).await.unwrap();
    writer.write(Bytes::from_static(b"data")).await.unwrap();
    drop(writer);
    assert_eq!(store.staging_count(), 0);
    assert!(store.is_empty());
}

#[tokio::test]
async fn abandoned_staging_is_listed_when_stale_and_cleared_by_delete() {
    let clock = FixedClock::default();
    let store = MemBlobStore::with_clock(Arc::new(clock.clone()));
    store.set_keep_abandoned_staging(true);
    let mut writer = store.begin(&key("crashed"), None).await.unwrap();
    writer.write(Bytes::from_static(b"half")).await.unwrap();
    drop(writer);
    assert_eq!(store.staging_count(), 1);
    let hour = Duration::from_secs(3600);
    assert_eq!(store.list_stale_staging(hour).await.unwrap().len(), 0);
    clock.advance(hour);
    assert_eq!(
        store.list_stale_staging(hour).await.unwrap(),
        [key("crashed")]
    );
    assert!(!store.contains(&key("crashed")));
    store.delete(&key("crashed")).await.unwrap();
    assert_eq!(store.staging_count(), 0);
    let leftover = store.list_stale_staging(Duration::ZERO).await.unwrap();
    assert_eq!(leftover.len(), 0);
}

#[tokio::test]
async fn stale_staging_lists_each_key_once_and_only_old_enough_entries() {
    let clock = FixedClock::default();
    let store = MemBlobStore::with_clock(Arc::new(clock.clone()));
    let first = store.begin(&key("same"), None).await.unwrap();
    let second = store.begin(&key("same"), None).await.unwrap();
    clock.advance(Duration::from_secs(120));
    let young = store.begin(&key("young"), None).await.unwrap();
    let stale = store
        .list_stale_staging(Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(stale, [key("same")]);
    drop((first, second, young));
}

#[tokio::test]
async fn reads_come_in_8_kib_chunks_and_report_the_full_size() {
    let store = MemBlobStore::new();
    let k = key("big");
    store.put(&k, vec![1_u8; 20_000]);
    let read = store.get(&k, None).await.unwrap();
    assert_eq!(read.total_size, 20_000);
    let sizes: Vec<usize> = read.stream.map(|c| c.unwrap().len()).collect().await;
    assert_eq!(sizes, [8192, 8192, 3616]);
}

#[tokio::test]
async fn empty_object_reads_as_an_empty_stream() {
    let store = MemBlobStore::new();
    store.put(&key("e"), Bytes::new());
    let read = store.get(&key("e"), None).await.unwrap();
    assert_eq!(read.total_size, 0);
    assert_eq!(collect_stream(read).await.len(), 0);
}

#[tokio::test]
async fn ranges_are_inclusive_clamped_and_report_the_full_size() {
    let store = MemBlobStore::new();
    let k = key("digits");
    store.put(&k, Bytes::from_static(b"0123456789"));
    let cases = [
        (
            ByteRange {
                start: 2,
                end: Some(4),
            },
            &b"234"[..],
        ),
        (
            ByteRange {
                start: 7,
                end: None,
            },
            b"789",
        ),
        (
            ByteRange {
                start: 0,
                end: Some(0),
            },
            b"0",
        ),
        (
            ByteRange {
                start: 8,
                end: Some(999),
            },
            b"89",
        ),
        (
            ByteRange {
                start: 9,
                end: Some(9),
            },
            b"9",
        ),
    ];
    for (range, expected) in cases {
        let read = store.get(&k, Some(range)).await.unwrap();
        assert_eq!(read.total_size, 10, "{range:?}");
        assert_eq!(collect_stream(read).await, expected, "{range:?}");
    }
}

#[tokio::test]
async fn unsatisfiable_ranges_are_rejected() {
    let store = MemBlobStore::new();
    let k = key("digits");
    store.put(&k, Bytes::from_static(b"0123456789"));
    store.put(&key("empty"), Bytes::new());
    let bad = [
        (
            k.clone(),
            ByteRange {
                start: 10,
                end: None,
            },
        ),
        (
            k.clone(),
            ByteRange {
                start: 99,
                end: Some(120),
            },
        ),
        (
            k.clone(),
            ByteRange {
                start: 5,
                end: Some(4),
            },
        ),
        (
            key("empty"),
            ByteRange {
                start: 0,
                end: None,
            },
        ),
    ];
    for (target, range) in bad {
        let result = store.get(&target, Some(range)).await;
        assert!(
            matches!(result, Err(BlobError::RangeNotSatisfiable)),
            "{range:?}"
        );
    }
}

#[tokio::test]
async fn missing_key_is_not_found_for_get_and_none_for_head_and_delete_succeeds() {
    let store = MemBlobStore::new();
    let missing = key("missing");
    assert!(matches!(
        store.get(&missing, None).await,
        Err(BlobError::NotFound)
    ));
    assert!(store.head(&missing).await.unwrap().is_none());
    store.delete(&missing).await.unwrap();
}

#[tokio::test]
async fn commit_to_an_existing_key_swaps_the_content_atomically() {
    let store = MemBlobStore::new();
    let k = key("swap");
    write_all(&store, "swap", &[b"old"]).await;
    let mut writer = store.begin(&k, None).await.unwrap();
    writer
        .write(Bytes::from_static(b"new content"))
        .await
        .unwrap();
    assert_eq!(store.snapshot(&k).unwrap(), Bytes::from_static(b"old"));
    writer.commit().await.unwrap();
    assert_eq!(
        store.snapshot(&k).unwrap(),
        Bytes::from_static(b"new content")
    );
    assert_eq!(store.len(), 1);
}

#[tokio::test]
async fn delete_removes_the_object() {
    let store = MemBlobStore::new();
    let k = key("gone");
    store.put(&k, Bytes::from_static(b"x"));
    store.delete(&k).await.unwrap();
    assert!(!store.contains(&k));
}

#[tokio::test]
async fn keys_are_sorted_and_clones_share_objects() {
    let store = MemBlobStore::new();
    let alias = store.clone();
    store.put(&key("b"), Bytes::from_static(b"1"));
    alias.put(&key("a"), Bytes::from_static(b"2"));
    assert_eq!(store.keys(), [key("a"), key("b")]);
    assert_eq!(alias.len(), 2);
    assert!(format!("{store:?}").contains("objects: 2"));
}

#[tokio::test]
async fn put_returns_the_digests_of_the_content() {
    let store = MemBlobStore::new();
    let commit = store.put(&key("p"), Bytes::from_static(b"abc"));
    assert_eq!(commit.size_bytes, 3);
    assert_eq!(commit.checksums.md5, "900150983cd24fb0d6963f7d28e17f72");
}

#[tokio::test]
async fn kind_is_local_and_health_is_ok() {
    let store = MemBlobStore::default();
    assert_eq!(store.kind(), BlobStoreKind::Local);
    store.health().await.unwrap();
}

#[tokio::test]
async fn unavailable_store_fails_every_operation() {
    let store = MemBlobStore::new();
    let k = key("k");
    store.put(&k, Bytes::from_static(b"x"));
    store.set_unavailable(true);
    assert!(matches!(
        store.begin(&k, None).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        store.get(&k, None).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        store.head(&k).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        store.delete(&k).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        store.list_stale_staging(Duration::ZERO).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        store.health().await,
        Err(BlobError::Unavailable(_))
    ));
    store.set_unavailable(false);
    store.health().await.unwrap();
}

#[tokio::test]
async fn must_not_publish_an_object_when_the_store_fails_during_the_write() {
    let store = MemBlobStore::new();
    let k = key("flaky");
    let mut writer = store.begin(&k, None).await.unwrap();
    writer.write(Bytes::from_static(b"first")).await.unwrap();
    store.set_unavailable(true);
    assert!(matches!(
        writer.write(Bytes::from_static(b"second")).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        writer.commit().await,
        Err(BlobError::Unavailable(_))
    ));
    store.set_unavailable(false);
    assert!(!store.contains(&k));
    assert_eq!(store.staging_count(), 0);
}

#[tokio::test]
async fn open_read_keeps_its_data_after_the_object_is_deleted() {
    let store = MemBlobStore::new();
    let k = key("owned");
    store.put(&k, vec![5_u8; 10_000]);
    let mut read = store.get(&k, None).await.unwrap();
    store.delete(&k).await.unwrap();
    let first = read.stream.next().await.unwrap().unwrap();
    assert_eq!(first.len(), 8192);
}

// capabilities, listing, presigning and resumable uploads

#[tokio::test]
async fn capabilities_name_what_the_double_can_do_and_shutdown_is_idempotent() {
    let store = MemBlobStore::new();
    let caps = store.capabilities();
    assert!(caps.lists_staging && caps.lists_keys && caps.resumable);
    assert!(
        !caps.can_presign,
        "presigning is off until a test turns it on"
    );
    store.set_presign_enabled(true);
    assert!(store.capabilities().can_presign);
    store.shutdown().await;
    store.shutdown().await;
    assert!(store.resumable().is_some());
}

#[tokio::test]
async fn list_keys_pages_in_a_stable_order_with_an_opaque_cursor() {
    let store = MemBlobStore::new();
    for name in ["c", "a", "b", "d"] {
        store.put(&key(name), name.as_bytes().to_vec());
    }
    let first = store.list_keys(None, 3).await.unwrap();
    assert_eq!(first.keys.len(), 3);
    let cursor = first.next.clone().unwrap();
    let rest = store.list_keys(Some(&cursor), 3).await.unwrap();
    assert_eq!(rest.keys.len(), 1);
    assert!(rest.next.is_none());
    let mut seen: Vec<&str> = first
        .keys
        .iter()
        .chain(&rest.keys)
        .map(|(k, _)| k.as_str())
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, ["a", "b", "c", "d"], "no key is skipped or repeated");
    assert!(
        first
            .keys
            .iter()
            .all(|(_, meta)| meta.size_bytes == 1 && meta.modified.is_some())
    );
    // A limit of zero still makes progress, and an empty store answers an empty page.
    assert_eq!(store.list_keys(None, 0).await.unwrap().keys.len(), 1);
    assert!(
        MemBlobStore::new()
            .list_keys(None, 10)
            .await
            .unwrap()
            .keys
            .is_empty()
    );
    for bad in ["garbage", "k1.zz", "k1.ff"] {
        let error = store.list_keys(Some(bad), 10).await.unwrap_err();
        assert!(matches!(error, BlobError::InvalidKey(_)), "{bad}");
    }
    store.set_unavailable(true);
    assert!(matches!(
        store.list_keys(None, 1).await,
        Err(BlobError::Unavailable(_))
    ));
}

#[tokio::test]
async fn presigning_answers_a_secret_url_only_when_enabled() {
    let store = MemBlobStore::new();
    let options = PresignOptions::default();
    let ttl = Duration::from_secs(300);
    assert!(
        store
            .presign_get(&key("k"), ttl, &options)
            .await
            .unwrap()
            .is_none()
    );
    store.set_presign_enabled(true);
    let signed = store
        .presign_get(&key("k"), ttl, &options)
        .await
        .unwrap()
        .unwrap();
    assert!(signed.url.expose().contains("X-Amz-Signature"));
    assert!(
        !format!("{signed:?}").contains("X-Amz-Signature"),
        "a presigned URL is a secret in Debug"
    );
    assert!(signed.expires_at > time::OffsetDateTime::now_utc());
    store.set_unavailable(true);
    assert!(matches!(
        store.presign_get(&key("k"), ttl, &options).await,
        Err(BlobError::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_resumable_upload_stages_chunks_at_the_expected_offset_and_commits_on_finalize() {
    let store = MemBlobStore::new();
    let resumable = store.resumable().unwrap();
    let handle = resumable.begin_resumable(&key("blob")).await.unwrap();
    assert_eq!(store.open_uploads(), 1);
    assert_eq!(resumable.length(&handle).await.unwrap(), 0);
    resumable
        .append(&handle, 0, Bytes::from_static(b"hel"))
        .await
        .unwrap();
    resumable
        .append(&handle, 3, Bytes::from_static(b"lo"))
        .await
        .unwrap();
    assert_eq!(resumable.length(&handle).await.unwrap(), 5);
    // A wrong offset is refused and stages nothing: the client resumes from the length.
    let wrong = resumable
        .append(&handle, 9, Bytes::from_static(b"!"))
        .await
        .unwrap_err();
    assert!(matches!(wrong, BlobError::RangeNotSatisfiable));
    assert_eq!(resumable.length(&handle).await.unwrap(), 5);
    assert!(!store.contains(&key("blob")), "invisible until finalize");

    let commit = resumable.finalize(handle.clone()).await.unwrap();
    assert_eq!(commit.size_bytes, 5);
    assert_eq!(commit.checksums, checksums_of(b"hello"));
    assert_eq!(
        store.snapshot(&key("blob")).unwrap(),
        Bytes::from_static(b"hello")
    );
    assert_eq!(store.open_uploads(), 0);
    assert_eq!(store.staging_count(), 0);
    assert!(matches!(
        resumable.length(&handle).await,
        Err(BlobError::NotFound)
    ));
    assert!(matches!(
        resumable.finalize(handle.clone()).await,
        Err(BlobError::NotFound)
    ));
    assert!(matches!(
        resumable.append(&handle, 0, Bytes::new()).await,
        Err(BlobError::NotFound)
    ));
    assert!(matches!(
        UploadHandle::new("bad handle!"),
        Err(BlobError::NotFound)
    ));
}

#[tokio::test]
async fn an_abandoned_upload_is_listed_by_the_staging_sweep_and_aborts_cleanly() {
    let clock = Arc::new(FixedClock::default());
    let store = MemBlobStore::with_clock(clock.clone());
    let resumable = store.resumable().unwrap();
    let abandoned = resumable
        .begin_resumable(&key("left-behind"))
        .await
        .unwrap();
    resumable
        .append(&abandoned, 0, Bytes::from_static(b"x"))
        .await
        .unwrap();
    let aborted = resumable.begin_resumable(&key("aborted")).await.unwrap();
    resumable.abort(aborted.clone()).await.unwrap();
    resumable.abort(aborted).await.unwrap();
    assert!(
        store
            .list_stale_staging(Duration::from_secs(60))
            .await
            .unwrap()
            .is_empty(),
        "too young"
    );
    clock.advance(Duration::from_secs(120));
    let stale = store
        .list_stale_staging(Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(stale, [key("left-behind")]);
    // The sweep deletes by key, which also removes the upload behind it.
    store.delete(&key("left-behind")).await.unwrap();
    assert_eq!(store.open_uploads(), 0);
    assert!(matches!(
        resumable.length(&abandoned).await,
        Err(BlobError::NotFound)
    ));
    store.set_unavailable(true);
    assert!(matches!(
        resumable.begin_resumable(&key("k")).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        resumable.abort(abandoned.clone()).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        resumable.finalize(abandoned.clone()).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        resumable.length(&abandoned).await,
        Err(BlobError::Unavailable(_))
    ));
    assert!(matches!(
        resumable.append(&abandoned, 0, Bytes::new()).await,
        Err(BlobError::Unavailable(_))
    ));
}
