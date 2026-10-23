//! `list_keys` against the in-memory store.

use object_store::PutPayload;
use rustryx_core::ports::BlobStore;

use super::*;
use crate::key::{MAX_LIST_PAGE, relative_location};

fn names(page: &KeyPage) -> Vec<String> {
    page.keys
        .iter()
        .map(|(key, _)| key.as_str().to_owned())
        .collect()
}

async fn walk(store: &S3BlobStore, limit: u32) -> (Vec<String>, usize) {
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

async fn put_raw(fx: &Fixture, name: &str) {
    fx.target
        .put(&ObjectPath::from(name), PutPayload::from_static(b"raw"))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_empty_store_lists_an_empty_page_without_a_cursor() {
    let fx = fixture();
    let page = fx.store.list_keys(None, 10).await.unwrap();
    assert_eq!(page.keys.len(), 0);
    assert!(page.next.is_none());
}

#[tokio::test]
async fn pages_cover_every_key_once_in_location_order() {
    let fx = fixture();
    let wanted: Vec<String> = (0..7).map(|i| format!("blob-{i}")).collect();
    for name in &wanted {
        put(&fx, name, &pattern(10), 10).await;
    }
    let (seen, pages) = walk(&fx.store, 3).await;
    assert_eq!(pages, 3, "7 keys in pages of 3");
    let mut sorted = seen.clone();
    sorted.sort();
    let mut expected = wanted.clone();
    expected.sort();
    assert_eq!(sorted, expected, "every key exactly once");
    let locations: Vec<String> = seen.iter().map(|k| relative_location(k)).collect();
    assert!(
        locations.windows(2).all(|pair| pair[0] < pair[1]),
        "stable, strictly increasing order of locations"
    );
}

#[tokio::test]
async fn a_page_that_ends_exactly_at_the_last_key_has_no_cursor() {
    let fx = fixture();
    for i in 0..4 {
        put(&fx, &format!("k{i}"), &pattern(1), 1).await;
    }
    let all = fx.store.list_keys(None, 4).await.unwrap();
    assert_eq!(all.keys.len(), 4);
    assert!(all.next.is_none(), "no phantom empty page");
    let (_, pages) = walk(&fx.store, 2).await;
    assert_eq!(pages, 2);
}

#[tokio::test]
async fn the_limit_is_clamped_to_one_and_to_the_page_maximum() {
    let fx = fixture();
    for i in 0..3 {
        put(&fx, &format!("c{i}"), &pattern(1), 1).await;
    }
    let one = fx.store.list_keys(None, 0).await.unwrap();
    assert_eq!(one.keys.len(), 1, "zero is raised to one");
    assert!(one.next.is_some());

    for i in 0..(MAX_LIST_PAGE as usize + 5) {
        let name = format!("many-{i}");
        fx.target
            .put(
                &ObjectPath::from(relative_location(&name)),
                PutPayload::from_static(b"x"),
            )
            .await
            .unwrap();
    }
    let big = fx.store.list_keys(None, u32::MAX).await.unwrap();
    assert_eq!(big.keys.len(), MAX_LIST_PAGE as usize);
    assert!(big.next.is_some());
}

#[tokio::test]
async fn foreign_objects_are_not_blobs() {
    let fx = fixture();
    put(&fx, "genuine", &pattern(5), 5).await;
    put_raw(&fx, "foreign.txt").await;
    put_raw(&fx, "_rustryx/health-probe").await;
    // Right shape, wrong shard: no `get` could ever reach it.
    put_raw(&fx, "00/00/genuine-in-the-wrong-shard").await;
    put_raw(&fx, &format!("{}/with space", relative_location("x"))).await;
    let page = fx.store.list_keys(None, 100).await.unwrap();
    assert_eq!(names(&page), vec!["genuine".to_owned()]);
}

#[tokio::test]
async fn only_the_store_prefix_is_listed() {
    let fx = fixture_with("tenant", settings(2));
    put(&fx, "mine", &pattern(5), 5).await;
    put_raw(&fx, &format!("other/{}", relative_location("theirs"))).await;
    put_raw(&fx, &relative_location("unprefixed")).await;
    let page = fx.store.list_keys(None, 100).await.unwrap();
    assert_eq!(names(&page), vec!["mine".to_owned()]);
}

#[tokio::test]
async fn the_cursor_survives_the_deletion_of_its_own_key() {
    let fx = fixture();
    for i in 0..5 {
        put(&fx, &format!("d{i}"), &pattern(1), 1).await;
    }
    let first = fx.store.list_keys(None, 2).await.unwrap();
    let cursor = first.next.clone().unwrap();
    let last = first.keys.last().unwrap().0.clone();
    fx.store.delete(&last).await.unwrap();
    let rest = fx.store.list_keys(Some(&cursor), 10).await.unwrap();
    assert_eq!(rest.keys.len(), 3);
    assert!(rest.keys.iter().all(|(k, _)| *k != last));
}

#[tokio::test]
async fn entries_carry_size_and_modification_time() {
    let fx = fixture();
    put(&fx, "sized", &pattern(123), 50).await;
    let page = fx.store.list_keys(None, 10).await.unwrap();
    let (_, meta) = &page.keys[0];
    assert_eq!(meta.size_bytes, 123);
    assert!(meta.modified.is_some());
    let head = fx.store.head(&key("sized")).await.unwrap().unwrap();
    assert_eq!(head.size_bytes, 123);
    assert!(head.modified.is_some());
}

#[tokio::test]
async fn must_not_follow_a_hostile_cursor() {
    let fx = fixture();
    let genuine = relative_location("genuine");
    for bad in [
        String::new(),
        "genuine".to_owned(),
        "../../etc/passwd".to_owned(),
        "00/00/../../x".to_owned(),
        "00/00/genuine".to_owned(),
        format!("{genuine}/"),
        format!("{genuine}\n"),
    ] {
        assert!(
            matches!(
                fx.store.list_keys(Some(&bad), 10).await,
                Err(BlobError::InvalidKey(_))
            ),
            "{bad:?}"
        );
    }
}

/// A backend whose listing is scripted: names in the given order, or a failure. `template`
/// supplies a real `ObjectMeta` (the test crate names no time library of its own).
#[derive(Debug)]
struct Scripted {
    names: Vec<String>,
    fail: bool,
    template: object_store::ObjectMeta,
}

async fn scripted(names: &[&str], fail: bool) -> Scripted {
    let memory = InMemory::new();
    let path = ObjectPath::from("template");
    memory
        .put(&path, PutPayload::from_static(b"t"))
        .await
        .unwrap();
    Scripted {
        names: names.iter().map(|n| (*n).to_owned()).collect(),
        fail,
        template: memory.head(&path).await.unwrap(),
    }
}

fn store_over(backend: Scripted, fx: &Fixture) -> S3BlobStore {
    S3BlobStore::from_backends(
        Arc::new(backend),
        Arc::clone(&fx.fake) as Arc<dyn MultipartStore>,
        "",
        settings(1),
    )
    .unwrap()
}

impl std::fmt::Display for Scripted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("scripted")
    }
}

#[async_trait]
impl ObjectStore for Scripted {
    async fn put_opts(
        &self,
        _: &ObjectPath,
        _: PutPayload,
        _: object_store::PutOptions,
    ) -> object_store::Result<object_store::PutResult> {
        Err(injected("put"))
    }
    async fn put_multipart_opts(
        &self,
        _: &ObjectPath,
        _: object_store::PutMultipartOptions,
    ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
        Err(injected("multipart"))
    }
    async fn get_opts(
        &self,
        _: &ObjectPath,
        _: GetOptions,
    ) -> object_store::Result<object_store::GetResult> {
        Err(injected("get"))
    }
    fn delete_stream(
        &self,
        _: futures::stream::BoxStream<'static, object_store::Result<ObjectPath>>,
    ) -> futures::stream::BoxStream<'static, object_store::Result<ObjectPath>> {
        futures::stream::empty().boxed()
    }
    fn list(
        &self,
        _: Option<&ObjectPath>,
    ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>> {
        if self.fail {
            return futures::stream::once(async { Err(injected("list")) }).boxed();
        }
        let items: Vec<_> = self
            .names
            .iter()
            .map(|name| {
                Ok(object_store::ObjectMeta {
                    location: ObjectPath::from(name.as_str()),
                    ..self.template.clone()
                })
            })
            .collect();
        futures::stream::iter(items).boxed()
    }
    async fn list_with_delimiter(
        &self,
        _: Option<&ObjectPath>,
    ) -> object_store::Result<object_store::ListResult> {
        Err(injected("delimiter"))
    }
    async fn copy_opts(
        &self,
        _: &ObjectPath,
        _: &ObjectPath,
        _: object_store::CopyOptions,
    ) -> object_store::Result<()> {
        Err(injected("copy"))
    }
}

#[tokio::test]
async fn a_listing_error_surfaces_instead_of_an_empty_page() {
    // An empty page here would read as "the store is empty", which verify_blobs must never
    // conclude from an outage.
    let fx = fixture();
    let store = store_over(scripted(&[], true).await, &fx);
    assert!(matches!(
        store.list_keys(None, 10).await,
        Err(BlobError::Unavailable(_))
    ));
}

#[tokio::test]
async fn must_not_page_through_a_backend_that_lists_out_of_order() {
    // The cursor is the last name of a page; with names out of order a walk would repeat
    // or skip keys and never say so.
    let fx = fixture();
    let a = relative_location("aaa");
    let b = relative_location("bbb");
    let (low, high) = if a < b { (a, b) } else { (b, a) };
    let store = store_over(scripted(&[&high, &low], false).await, &fx);
    assert!(matches!(
        store.list_keys(None, 10).await,
        Err(BlobError::Backend {
            context: "list objects",
            ..
        })
    ));
    let store = store_over(scripted(&[&low, &low], false).await, &fx);
    assert!(
        store.list_keys(None, 10).await.is_err(),
        "a repeated name is out of order too"
    );
    let store = store_over(scripted(&[&low, &high], false).await, &fx);
    assert_eq!(store.list_keys(None, 10).await.unwrap().keys.len(), 2);
}
