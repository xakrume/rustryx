//! `list_keys` of the local store: the shard walk, its cursor and its bounds.
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::Bytes;
use rustryx_blob::{LocalBlobStore, shard_of};
use rustryx_core::BlobError;
use rustryx_core::ports::{BlobKey, BlobStore, KeyPage};
use std::path::PathBuf;

async fn open() -> (tempfile::TempDir, LocalBlobStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalBlobStore::open(dir.path().join("blobs"))
        .await
        .unwrap();
    (dir, store)
}

fn key(name: &str) -> BlobKey {
    BlobKey::new(name)
}

async fn put(store: &LocalBlobStore, name: &str, body: &'static [u8]) {
    let mut writer = store.begin(&key(name), None).await.unwrap();
    writer.write(Bytes::from_static(body)).await.unwrap();
    writer.commit().await.unwrap();
}

/// Where the store keeps a key.
fn location(store: &LocalBlobStore, name: &str) -> PathBuf {
    let (first, second) = shard_of(name);
    let mut path = store.root().join(first).join(second);
    path.extend(name.split('/'));
    path
}

/// Writes a blob file without going through the store, so that a test can create many.
async fn plant(store: &LocalBlobStore, name: &str) {
    let path = location(store, name);
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&path, b"x").await.unwrap();
}

fn names(page: &KeyPage) -> Vec<String> {
    page.keys
        .iter()
        .map(|(k, _)| k.as_str().to_owned())
        .collect()
}

async fn walk(store: &LocalBlobStore, limit: u32) -> (Vec<String>, usize) {
    let mut seen = Vec::new();
    let mut pages = 0;
    let mut after: Option<String> = None;
    loop {
        let page = store.list_keys(after.as_deref(), limit).await.unwrap();
        assert!(page.keys.len() <= limit.max(1) as usize);
        pages += 1;
        seen.extend(names(&page));
        match page.next {
            Some(next) => after = Some(next),
            None => return (seen, pages),
        }
    }
}

#[tokio::test]
async fn an_empty_store_lists_an_empty_page_without_a_cursor() {
    let (_dir, store) = open().await;
    let page = store.list_keys(None, 10).await.unwrap();
    assert_eq!(page.keys.len(), 0);
    assert!(page.next.is_none());
}

#[tokio::test]
async fn pages_cover_every_key_once_in_a_stable_order() {
    let (_dir, store) = open().await;
    let wanted: Vec<String> = (0..9).map(|i| format!("blob-{i}")).collect();
    for name in &wanted {
        put(&store, name, b"data").await;
    }
    let (first_walk, pages) = walk(&store, 4).await;
    assert_eq!(pages, 3, "9 keys in pages of 4");
    let mut sorted = first_walk.clone();
    sorted.sort();
    let mut expected = wanted.clone();
    expected.sort();
    assert_eq!(sorted, expected, "every key exactly once");

    // The same walk with another page size visits the same keys in the same order, which
    // is what makes the cursor meaningful.
    let (second_walk, _) = walk(&store, 2).await;
    assert_eq!(first_walk, second_walk);
    let (one_page, one) = walk(&store, 100).await;
    assert_eq!(one, 1);
    assert_eq!(first_walk, one_page);
}

#[tokio::test]
async fn a_page_that_ends_exactly_at_the_last_key_has_no_cursor() {
    let (_dir, store) = open().await;
    for i in 0..4 {
        put(&store, &format!("k{i}"), b"x").await;
    }
    let page = store.list_keys(None, 4).await.unwrap();
    assert_eq!(page.keys.len(), 4);
    assert!(page.next.is_none(), "no phantom empty page");
    let (_, pages) = walk(&store, 2).await;
    assert_eq!(pages, 2);
}

#[tokio::test]
async fn nested_keys_are_listed_with_their_structure() {
    let (_dir, store) = open().await;
    put(&store, "_uploads/docker/0192f3a4-0001", b"chunk").await;
    put(&store, "plain", b"x").await;
    let (seen, _) = walk(&store, 10).await;
    assert!(seen.contains(&"_uploads/docker/0192f3a4-0001".to_owned()));
    assert!(seen.contains(&"plain".to_owned()));
}

#[tokio::test]
async fn staging_is_not_part_of_the_listing() {
    let (_dir, store) = open().await;
    put(&store, "committed", b"x").await;
    let mut writer = store.begin(&key("in-flight"), None).await.unwrap();
    writer.write(Bytes::from_static(b"half")).await.unwrap();
    let (seen, _) = walk(&store, 10).await;
    assert_eq!(seen, vec!["committed".to_owned()]);
    writer.abort().await.unwrap();
}

#[tokio::test]
async fn entries_carry_size_and_a_recent_modification_time() {
    let (_dir, store) = open().await;
    put(&store, "sized", b"twelve bytes").await;
    let page = store.list_keys(None, 10).await.unwrap();
    let (_, meta) = &page.keys[0];
    assert_eq!(meta.size_bytes, 12);
    let modified = meta.modified.expect("a file system reports mtime");
    let age = time::OffsetDateTime::now_utc() - modified;
    assert!(age < time::Duration::minutes(5), "{age}");
    let head = store.head(&key("sized")).await.unwrap().unwrap();
    assert_eq!(head.modified, meta.modified);
}

#[tokio::test]
async fn the_cursor_survives_the_deletion_of_its_own_key() {
    let (_dir, store) = open().await;
    for i in 0..6 {
        put(&store, &format!("d{i}"), b"x").await;
    }
    let first = store.list_keys(None, 3).await.unwrap();
    let cursor = first.next.clone().unwrap();
    let last = first.keys.last().unwrap().0.clone();
    store.delete(&last).await.unwrap();
    let rest = store.list_keys(Some(&cursor), 10).await.unwrap();
    assert_eq!(rest.keys.len(), 3);
    assert!(rest.keys.iter().all(|(k, _)| *k != last));
}

#[tokio::test]
async fn the_limit_is_clamped_to_one_and_to_the_page_maximum() {
    let (_dir, store) = open().await;
    for i in 0..3 {
        plant(&store, &format!("c{i}")).await;
    }
    let one = store.list_keys(None, 0).await.unwrap();
    assert_eq!(one.keys.len(), 1, "zero is raised to one");
    assert!(one.next.is_some());

    for i in 0..1005 {
        plant(&store, &format!("many-{i}")).await;
    }
    let big = store.list_keys(None, u32::MAX).await.unwrap();
    assert_eq!(big.keys.len(), 1000);
    assert!(big.next.is_some());
    let (all, _) = walk(&store, u32::MAX).await;
    assert_eq!(all.len(), 1008);
}

#[tokio::test]
async fn must_not_report_files_that_are_not_blobs_of_their_shard() {
    let (_dir, store) = open().await;
    put(&store, "genuine", b"x").await;

    // Junk at the root, in a non-shard directory and in a shard of its own.
    tokio::fs::write(store.root().join("README"), b"hi")
        .await
        .unwrap();
    tokio::fs::create_dir_all(store.root().join("lost+found"))
        .await
        .unwrap();
    tokio::fs::write(store.root().join("lost+found").join("x"), b"x")
        .await
        .unwrap();
    tokio::fs::create_dir_all(store.root().join("zz/zz"))
        .await
        .unwrap();
    tokio::fs::write(store.root().join("zz/zz/not-hex"), b"x")
        .await
        .unwrap();
    // A well-formed name filed under the wrong shard: `get` could never reach it.
    let (first, second) = shard_of("misfiled");
    let wrong = if first == "00" { "01" } else { "00" };
    let dir = store.root().join(wrong).join(&second);
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("misfiled"), b"x").await.unwrap();
    // A name that is no valid key at all.
    let (first, second) = shard_of("genuine");
    let genuine_dir = store.root().join(first).join(second);
    tokio::fs::write(genuine_dir.join("with space"), b"x")
        .await
        .unwrap();
    tokio::fs::write(genuine_dir.join("é"), b"x").await.unwrap();

    let (seen, _) = walk(&store, 10).await;
    assert_eq!(seen, vec!["genuine".to_owned()]);
}

#[cfg(unix)]
#[tokio::test]
async fn must_not_follow_symlinks_out_of_the_root() {
    let (dir, store) = open().await;
    put(&store, "genuine", b"x").await;
    let outside = dir.path().join("outside");
    tokio::fs::create_dir_all(&outside).await.unwrap();
    tokio::fs::write(outside.join("secret"), b"top secret")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("target-file"), b"top secret")
        .await
        .unwrap();

    // A symlink named like a shard directory, one named like a key, and one nested.
    std::os::unix::fs::symlink(&outside, store.root().join("aa")).unwrap();
    let (first, second) = shard_of("linked");
    let leaf = store.root().join(first).join(second);
    tokio::fs::create_dir_all(&leaf).await.unwrap();
    std::os::unix::fs::symlink(dir.path().join("target-file"), leaf.join("linked")).unwrap();
    let (first, second) = shard_of("genuine");
    let genuine_leaf = store.root().join(first).join(second);
    std::os::unix::fs::symlink(&outside, genuine_leaf.join("dir-link")).unwrap();

    let (seen, _) = walk(&store, 10).await;
    assert_eq!(
        seen,
        vec!["genuine".to_owned()],
        "no link is followed or listed"
    );
}

#[tokio::test]
async fn must_not_follow_a_hostile_cursor() {
    let (_dir, store) = open().await;
    put(&store, "genuine", b"x").await;
    let (first, second) = shard_of("genuine");
    let genuine = format!("{first}/{second}/genuine");
    for bad in [
        String::new(),
        "genuine".to_owned(),
        "../../etc/passwd".to_owned(),
        "00/00/../../x".to_owned(),
        format!("00/00/{}", "genuine"),
        format!("{genuine}/"),
        format!("{genuine}/../x"),
        "00/00/_staging/x".to_owned(),
    ] {
        let outcome = store.list_keys(Some(&bad), 10).await;
        assert!(
            matches!(outcome, Err(BlobError::InvalidKey(_))),
            "{bad:?} gave {outcome:?}"
        );
    }
    // The genuine location is accepted and resumes after it.
    let page = store.list_keys(Some(&genuine), 10).await.unwrap();
    assert_eq!(page.keys.len(), 0);
}

#[tokio::test]
async fn must_not_report_a_missing_root_as_an_empty_store() {
    let (dir, store) = open().await;
    put(&store, "genuine", b"x").await;
    tokio::fs::remove_dir_all(dir.path().join("blobs"))
        .await
        .unwrap();
    // `verify_blobs` would otherwise conclude that every row lost its object.
    assert!(matches!(
        store.list_keys(None, 10).await,
        Err(BlobError::Unavailable(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_directory_is_an_error_not_a_gap() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, store) = open().await;
    put(&store, "genuine", b"x").await;
    let (first, second) = shard_of("genuine");
    let leaf = store.root().join(first).join(second);
    tokio::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o000))
        .await
        .unwrap();
    let outcome = store.list_keys(None, 10).await;
    // Restore before asserting, so a failure does not leave a directory the temp dir
    // cannot clean up.
    tokio::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(0o755))
        .await
        .unwrap();
    // Root can read anything, which makes the refusal impossible to provoke there.
    if let Err(err) = outcome {
        assert!(matches!(err, BlobError::Backend { .. }), "{err:?}");
    }
}

#[tokio::test]
async fn the_depth_of_the_walk_is_bounded() {
    let (_dir, store) = open().await;
    put(&store, "genuine", b"x").await;
    // A directory chain deeper than any valid key can be (a key has at most 256
    // components) must not make the walk run away or fail.
    let (first, second) = shard_of("genuine");
    let mut deep = store.root().join(first).join(second).join("nest");
    for _ in 0..300 {
        deep.push("d");
    }
    tokio::fs::create_dir_all(&deep).await.unwrap();
    tokio::fs::write(deep.join("file"), b"x").await.unwrap();
    let (seen, _) = walk(&store, 10).await;
    assert_eq!(seen, vec!["genuine".to_owned()]);
}

mod paging_property {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        /// However the keys fall into shards and however the pages are cut, a walk visits
        /// every key exactly once, and in the same order as a single big page.
        #[test]
        fn paging_with_any_limit_visits_every_key_exactly_once(
            names in proptest::collection::btree_set("[a-z0-9]{1,12}", 0..40),
            limit in 1_u32..12,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (_dir, store) = open().await;
                for name in &names {
                    plant(&store, name).await;
                }
                let (walked, _) = walk(&store, limit).await;
                let (single, _) = walk(&store, 1000).await;
                prop_assert_eq!(&walked, &single);
                let unique: BTreeSet<_> = walked.iter().cloned().collect();
                prop_assert_eq!(walked.len(), unique.len());
                prop_assert_eq!(unique, names.clone());
                Ok(())
            })?;
        }
    }
}
