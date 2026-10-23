//! `S3BlobStore` against a real S3 server (`MinIO`) started with testcontainers.
//!
//! Gating:
//! - `RUSTRYX_IT=1`: the test is required; a missing Docker or image is a failure.
//! - `RUSTRYX_IT=0`: skipped.
//! - unset: runs when Docker answers, otherwise skips with a message.
//!
//! Image: `RUSTRYX_MINIO_IMAGE=name:tag` overrides the candidates. The default
//! candidates are `minio/minio:latest` and, because the community image was removed
//! from Docker Hub, `cgr.dev/chainguard/minio:latest` (the same server).
//!
//! One test drives all steps against one container so that the container start
//! happens once and so that the RSS measurement is not polluted by parallel tests.
// Tests may unwrap, expect and panic: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::{byte_at, pattern, rss_kib};
use futures::{StreamExt, TryStreamExt};
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use rustryx_blob::{
    BlobHasher, BlobStoreRegistry, MapSecretResolver, S3BlobStore, S3Config, S3Credentials,
    shard_of,
};
use rustryx_core::error::StoreError;
use rustryx_core::ids::BlobStoreId;
use rustryx_core::model::{BlobStoreDef, BlobStoreKind};
use rustryx_core::ports::{
    BlobCommit, BlobKey, BlobStore, BlobStoreRepo, ByteRange, PresignOptions,
};
use rustryx_core::{BlobError, Secret};
use serde_json::json;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, CopyTargetOptions, GenericImage, ImageExt};

const BUCKET: &str = "artifacts";
const ACCESS_KEY: &str = "rustryx-it-user";
const SECRET_KEY: &str = "rustryx-it-password";
const MIB: usize = 1024 * 1024;
const CHUNK: usize = 64 * 1024;

enum Gate {
    Required,
    Skip(&'static str),
    Probe,
}

fn gate() -> Gate {
    match std::env::var("RUSTRYX_IT").as_deref() {
        Ok("1") => Gate::Required,
        Ok("0") => Gate::Skip("RUSTRYX_IT=0"),
        _ => Gate::Probe,
    }
}

fn docker_answers() -> bool {
    std::process::Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .is_ok_and(|out| out.status.success())
}

fn image_candidates() -> Vec<String> {
    match std::env::var("RUSTRYX_MINIO_IMAGE") {
        Ok(image) if !image.is_empty() => vec![image],
        _ => vec![
            "minio/minio:latest".into(),
            "cgr.dev/chainguard/minio:latest".into(),
        ],
    }
}

async fn start_one(image: &str) -> Result<ContainerAsync<GenericImage>, String> {
    let (name, tag) = image.rsplit_once(':').unwrap_or((image, "latest"));
    GenericImage::new(name, tag)
        .with_exposed_port(9000.tcp())
        // `MinIO` prints its banner (with the API address) on stderr.
        .with_wait_for(WaitFor::message_on_stderr("API: http"))
        .with_env_var("MINIO_ROOT_USER", ACCESS_KEY)
        .with_env_var("MINIO_ROOT_PASSWORD", SECRET_KEY)
        // A top-level directory under the data dir is a bucket for `MinIO`; copying a
        // file into it creates the bucket before the server starts, which spares the
        // test a signed `CreateBucket` call. Root so that the data dir is writable.
        .with_user("0")
        .with_copy_to(
            CopyTargetOptions::new(format!("/data/{BUCKET}/.keep")),
            Vec::<u8>::new(),
        )
        .with_cmd(["server", "/data"])
        .start()
        .await
        .map_err(|err| format!("{image}: {err}"))
}

async fn start_minio() -> Result<ContainerAsync<GenericImage>, String> {
    let mut failures = Vec::new();
    for image in image_candidates() {
        match start_one(&image).await {
            Ok(container) => return Ok(container),
            Err(err) => failures.push(err),
        }
    }
    Err(failures.join("; "))
}

fn s3_config(endpoint: &str, extra: &serde_json::Value) -> S3Config {
    let mut value = json!({
        "bucket": BUCKET,
        "endpoint": endpoint,
        "allow_http": true,
        "prefix": "it",
    });
    if let (Some(base), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        base.extend(more.clone());
    }
    serde_json::from_value(value).unwrap()
}

fn credentials() -> S3Credentials {
    S3Credentials {
        access_key_id: Secret::new(ACCESS_KEY),
        secret_access_key: Secret::new(SECRET_KEY),
        session_token: None,
    }
}

fn key(name: &str) -> BlobKey {
    BlobKey::new(name)
}

fn expected(offset: u64, len: u64) -> BlobCommit {
    let mut hasher = BlobHasher::new();
    let mut at = 0;
    while at < len {
        let take = usize::try_from((len - at).min(CHUNK as u64)).unwrap();
        hasher.update(&pattern(offset + at, take));
        at += take as u64;
    }
    hasher.finish()
}

/// Streams `len` bytes of the test pattern into a new writer for `name`.
async fn upload(store: &S3BlobStore, name: &str, len: u64) -> BlobCommit {
    let mut writer = store.begin(&key(name), Some(len)).await.unwrap();
    let mut at = 0;
    while at < len {
        let take = usize::try_from((len - at).min(CHUNK as u64)).unwrap();
        writer.write(pattern(at, take)).await.unwrap();
        at += take as u64;
    }
    writer.commit().await.unwrap()
}

/// Reads an object to the end, verifying the pattern byte by byte against `start`.
async fn verify_download(
    store: &S3BlobStore,
    name: &str,
    range: Option<ByteRange>,
    start: u64,
    len: u64,
) {
    let read = store.get(&key(name), range).await.unwrap();
    let mut stream = read.stream;
    let mut position = start;
    while let Some(piece) = stream.next().await {
        let piece: Bytes = piece.unwrap();
        assert!(
            piece.len() <= rustryx_blob::READ_CHUNK_BYTES,
            "chunk of {} bytes",
            piece.len()
        );
        for (i, byte) in piece.iter().enumerate() {
            assert_eq!(
                *byte,
                byte_at(position + i as u64),
                "content differs at {}",
                position + i as u64
            );
        }
        position += piece.len() as u64;
    }
    assert_eq!(position - start, len, "length of {name}");
}

async fn wait_until_reachable(store: &S3BlobStore) {
    let mut last = None;
    for _ in 0..60 {
        match store.health().await {
            Ok(()) => return,
            Err(err) => last = Some(err),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("MinIO did not become reachable: {last:?}");
}

async fn small_object_round_trip(store: &S3BlobStore) {
    let commit = upload(store, "small", 2048).await;
    assert_eq!(commit, expected(0, 2048));
    let head = store.head(&key("small")).await.unwrap().unwrap();
    assert_eq!(head.size_bytes, 2048);
    assert!(head.modified.is_some(), "S3 reports a modification time");
    verify_download(store, "small", None, 0, 2048).await;
    verify_download(
        store,
        "small",
        Some(ByteRange {
            start: 100,
            end: Some(199),
        }),
        100,
        100,
    )
    .await;
    assert!(matches!(
        store
            .get(
                &key("small"),
                Some(ByteRange {
                    start: 2048,
                    end: None
                })
            )
            .await,
        Err(BlobError::RangeNotSatisfiable)
    ));
}

async fn multipart_round_trip(store: &S3BlobStore, part: usize) {
    // Three full parts and a tail, so the last part is smaller than the others.
    let len = (part * 3 + 12_345) as u64;
    let commit = upload(store, "multi", len).await;
    assert_eq!(commit, expected(0, len));
    assert_eq!(
        store.head(&key("multi")).await.unwrap().unwrap().size_bytes,
        len
    );
    verify_download(store, "multi", None, 0, len).await;
    // A range that straddles the first part boundary.
    let start = part as u64 - 5;
    verify_download(
        store,
        "multi",
        Some(ByteRange {
            start,
            end: Some(start + 9),
        }),
        start,
        10,
    )
    .await;

    // Exactly two parts: no empty trailing part may be sent (S3 would reject it).
    let exact = (part * 2) as u64;
    assert_eq!(upload(store, "exact", exact).await, expected(0, exact));
    verify_download(store, "exact", None, 0, exact).await;
}

async fn must_not_make_the_object_visible_before_commit(store: &S3BlobStore, part: usize) {
    let mut writer = store.begin(&key("invisible"), None).await.unwrap();
    let mut at = 0_u64;
    // More than one part is already stored remotely when we look.
    while at < (part as u64) * 2 + 1 {
        writer.write(pattern(at, CHUNK)).await.unwrap();
        at += CHUNK as u64;
    }
    assert!(store.head(&key("invisible")).await.unwrap().is_none());
    assert!(matches!(
        store.get(&key("invisible"), None).await,
        Err(BlobError::NotFound)
    ));
    writer.commit().await.unwrap();
    assert_eq!(
        store
            .head(&key("invisible"))
            .await
            .unwrap()
            .unwrap()
            .size_bytes,
        at
    );
}

async fn must_not_leave_an_object_after_abort_or_drop(store: &S3BlobStore, part: usize) {
    let mut writer = store.begin(&key("aborted"), None).await.unwrap();
    for i in 0..(part * 2 / CHUNK + 2) {
        writer
            .write(pattern((i * CHUNK) as u64, CHUNK))
            .await
            .unwrap();
    }
    // `abort` reports an error when the server does not know the upload, so an Ok
    // proves that the multipart upload existed and was removed.
    writer.abort().await.unwrap();
    assert!(store.head(&key("aborted")).await.unwrap().is_none());

    let mut writer = store.begin(&key("dropped"), None).await.unwrap();
    for i in 0..(part * 2 / CHUNK + 2) {
        writer
            .write(pattern((i * CHUNK) as u64, CHUNK))
            .await
            .unwrap();
    }
    drop(writer);
    store.shutdown().await;
    assert!(store.head(&key("dropped")).await.unwrap().is_none());
    assert_eq!(store.available_upload_slots(), 4);
}

async fn delete_and_missing(store: &S3BlobStore) {
    store.delete(&key("small")).await.unwrap();
    assert!(store.head(&key("small")).await.unwrap().is_none());
    store.delete(&key("small")).await.unwrap();
    store.delete(&key("never-existed")).await.unwrap();
    assert!(matches!(
        store.get(&key("never-existed"), None).await,
        Err(BlobError::NotFound)
    ));
}

async fn upload_slots_follow_the_writer_lifecycle(endpoint: &str) {
    let config = s3_config(endpoint, &json!({"max_concurrent_uploads": 2}));
    let store = S3BlobStore::new("slots", &config, Some(&credentials())).unwrap();
    let first = store.begin(&key("slot-a"), None).await.unwrap();
    let second = store.begin(&key("slot-b"), None).await.unwrap();
    assert_eq!(store.available_upload_slots(), 0);
    first.abort().await.unwrap();
    assert_eq!(store.available_upload_slots(), 1);
    second.commit().await.unwrap();
    assert_eq!(store.available_upload_slots(), 2);
    store.delete(&key("slot-b")).await.unwrap();
}

async fn memory_stays_bounded(store: &S3BlobStore) {
    let len = 160 * MIB as u64;
    let baseline = rss_kib();
    let mut peak = baseline;
    let mut writer = store.begin(&key("big"), Some(len)).await.unwrap();
    let mut at = 0_u64;
    let mut index = 0_u64;
    let chunk = pattern(0, CHUNK);
    while at < len {
        writer.write(chunk.clone()).await.unwrap();
        at += CHUNK as u64;
        index += 1;
        if index.is_multiple_of(128) {
            peak = peak.max(rss_kib());
        }
    }
    let commit = writer.commit().await.unwrap();
    assert_eq!(commit.size_bytes, len);

    let read = store.get(&key("big"), None).await.unwrap();
    let mut stream = read.stream;
    let mut seen = 0_u64;
    let mut pieces = 0_u64;
    while let Some(piece) = stream.try_next().await.unwrap() {
        seen += piece.len() as u64;
        pieces += 1;
        if pieces.is_multiple_of(256) {
            peak = peak.max(rss_kib());
        }
    }
    assert_eq!(seen, len);
    let growth_kib = peak.saturating_sub(baseline);
    assert!(
        growth_kib < 64 * 1024,
        "RSS grew by {} MiB while moving {} MiB through S3",
        growth_kib / 1024,
        len / MIB as u64
    );
    eprintln!(
        "S3: moved {} MiB each way, RSS growth {} KiB",
        len / MIB as u64,
        growth_kib
    );
    store.delete(&key("big")).await.unwrap();
}

struct OneStoreRepo(BlobStoreDef);

#[async_trait::async_trait]
impl BlobStoreRepo for OneStoreRepo {
    async fn create(&self, _: &BlobStoreDef) -> Result<(), StoreError> {
        Ok(())
    }
    async fn get(&self, id: BlobStoreId) -> Result<Option<BlobStoreDef>, StoreError> {
        Ok((id == self.0.id).then(|| self.0.clone()))
    }
    async fn get_by_name(&self, _: &str) -> Result<Option<BlobStoreDef>, StoreError> {
        Ok(None)
    }
    async fn list(&self) -> Result<Vec<BlobStoreDef>, StoreError> {
        Ok(vec![self.0.clone()])
    }
    async fn update(&self, _: &BlobStoreDef) -> Result<(), StoreError> {
        Ok(())
    }
    async fn delete(&self, _: BlobStoreId) -> Result<(), StoreError> {
        Ok(())
    }
}

/// The path production uses: definition JSON with secret references, resolved by the
/// registry, no static credentials in the JSON.
async fn registry_builds_a_working_store(endpoint: &str) {
    let def = BlobStoreDef::new(
        BlobStoreId::generate(),
        "minio",
        BlobStoreKind::S3,
        json!({
            "bucket": BUCKET,
            "endpoint": endpoint,
            "allow_http": true,
            "prefix": "registry",
            "access_key_id_ref": "IT_KEY",
            "secret_access_key_ref": "IT_SECRET",
        }),
        time::OffsetDateTime::UNIX_EPOCH,
    );
    let secrets = MapSecretResolver::new([("IT_KEY", ACCESS_KEY), ("IT_SECRET", SECRET_KEY)]);
    let registry = BlobStoreRegistry::new(Arc::new(OneStoreRepo(def.clone())), Arc::new(secrets));
    let store = registry.store_by_id(def.id).await.unwrap();
    assert_eq!(registry.definition(def.id).await.unwrap().name, "minio");
    store.health().await.unwrap();
    let mut writer = store.begin(&key("via-registry"), None).await.unwrap();
    writer
        .write(Bytes::from_static(b"hello registry"))
        .await
        .unwrap();
    let commit = writer.commit().await.unwrap();
    assert_eq!(commit.size_bytes, 14);
    let read = store.get(&key("via-registry"), None).await.unwrap();
    let body: Vec<Bytes> = read.stream.try_collect().await.unwrap();
    assert_eq!(body.concat(), b"hello registry");
    store.delete(&key("via-registry")).await.unwrap();
}

async fn wrong_credentials_are_reported_not_hidden(endpoint: &str) {
    let config = s3_config(endpoint, &json!({}));
    let bad = S3Credentials {
        access_key_id: Secret::new(ACCESS_KEY),
        secret_access_key: Secret::new("definitely-wrong"),
        session_token: None,
    };
    let store = S3BlobStore::new("bad-credentials", &config, Some(&bad)).unwrap();
    assert!(
        store.health().await.is_err(),
        "bad credentials must fail the health check"
    );
    let mut writer = store.begin(&key("denied"), None).await.unwrap();
    writer.write(Bytes::from_static(b"x")).await.unwrap();
    assert!(
        writer.commit().await.is_err(),
        "bad credentials must fail the commit"
    );
    assert!(store.head(&key("denied")).await.is_err());
}

/// What an HTTP client without credentials sees.
struct HttpReply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpReply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A minimal HTTP/1.1 GET over a plain TCP connection. The test crate has no HTTP client,
/// and a presigned URL has to be fetched by something that holds no credentials.
async fn http_get(url: &str) -> HttpReply {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let parsed = url::Url::parse(url).unwrap();
    let host = parsed.host_str().unwrap();
    let port = parsed.port_or_known_default().unwrap();
    let mut target = parsed.path().to_owned();
    if let Some(query) = parsed.query() {
        target.push('?');
        target.push_str(query);
    }
    let mut stream = tokio::net::TcpStream::connect((host, port)).await.unwrap();
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept-Encoding: identity\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .collect();
    HttpReply {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    }
}

fn raw_client(endpoint: &str) -> impl ObjectStore {
    AmazonS3Builder::new()
        .with_bucket_name(BUCKET)
        .with_region("us-east-1")
        .with_endpoint(endpoint)
        .with_allow_http(true)
        .with_virtual_hosted_style_request(false)
        .with_access_key_id(ACCESS_KEY)
        .with_secret_access_key(SECRET_KEY)
        .build()
        .unwrap()
}

async fn list_keys_walks_the_bucket_and_skips_foreign_objects(store: &S3BlobStore, endpoint: &str) {
    let caps = store.capabilities();
    assert!(caps.lists_keys && !caps.lists_staging && !caps.can_presign);

    // An object that this crate did not write, under the store's own prefix.
    let raw = raw_client(endpoint);
    let foreign = ObjectPath::from("it/foreign-object.txt");
    raw.put(&foreign, PutPayload::from_static(b"not a blob"))
        .await
        .unwrap();
    for name in ["list-a", "list-b", "list-c"] {
        upload(store, name, 100).await;
    }

    let mut seen: Vec<(String, u64, bool)> = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = store.list_keys(after.as_deref(), 2).await.unwrap();
        assert!(page.keys.len() <= 2, "a page never exceeds its limit");
        seen.extend(
            page.keys
                .iter()
                .map(|(k, m)| (k.as_str().to_owned(), m.size_bytes, m.modified.is_some())),
        );
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    for name in ["list-a", "list-b", "list-c"] {
        let entry = seen.iter().find(|(k, _, _)| k == name).unwrap();
        assert_eq!(entry.1, 100, "{name}");
        assert!(entry.2, "S3 reports a modification time for {name}");
    }
    assert!(
        seen.iter().all(|(k, _, _)| k != "foreign-object.txt"),
        "a foreign object is not a blob"
    );
    let locations: Vec<String> = seen
        .iter()
        .map(|(k, _, _)| {
            let (first, second) = shard_of(k);
            format!("{first}/{second}/{k}")
        })
        .collect();
    assert!(
        locations.windows(2).all(|pair| pair[0] < pair[1]),
        "the order is stable and strictly increasing by location: {locations:?}"
    );

    assert!(matches!(
        store.list_keys(Some("garbage"), 10).await,
        Err(BlobError::InvalidKey(_))
    ));
    raw.delete(&foreign).await.unwrap();
    for name in ["list-a", "list-b", "list-c"] {
        store.delete(&key(name)).await.unwrap();
    }
}

async fn presigned_downloads_work_without_credentials_and_expire(endpoint: &str) {
    let config = s3_config(
        endpoint,
        &json!({"presign": {"enabled": true, "ttl_secs": 60}}),
    );
    let store = S3BlobStore::new("presign", &config, Some(&credentials())).unwrap();
    assert!(store.capabilities().can_presign);
    upload(&store, "presigned", 4096).await;

    let options = PresignOptions {
        response_content_type: Some("application/x-rustryx-test".into()),
        response_content_disposition: Some("attachment; filename=\"p.bin\"".into()),
    };
    // A caller asking for an hour gets the configured minute, never more.
    let issued = time::OffsetDateTime::now_utc();
    let presigned = store
        .presign_get(&key("presigned"), Duration::from_secs(3600), &options)
        .await
        .unwrap()
        .unwrap();
    assert!(presigned.expires_at <= issued + Duration::from_secs(61));
    let url = presigned.url.expose().to_owned();
    assert!(url.contains("X-Amz-Expires=60"), "lifetime is in the URL");

    let reply = http_get(&url).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, pattern(0, 4096).to_vec());
    assert_eq!(
        reply.header("content-type"),
        Some("application/x-rustryx-test")
    );
    assert_eq!(
        reply.header("content-disposition"),
        Some("attachment; filename=\"p.bin\"")
    );

    // The overrides are inside the signature: a client cannot lift the `attachment`.
    let mut tampered = url::Url::parse(&url).unwrap();
    let pairs: Vec<(String, String)> = tampered
        .query_pairs()
        .map(|(k, v)| {
            if k == "response-content-disposition" {
                (k.into_owned(), "inline".to_owned())
            } else {
                (k.into_owned(), v.into_owned())
            }
        })
        .collect();
    tampered.query_pairs_mut().clear().extend_pairs(pairs);
    assert_eq!(http_get(tampered.as_str()).await.status, 403);

    // A URL for a key that is not there answers 404 at the client.
    let absent = store
        .presign_get(&key("presigned-absent"), Duration::from_secs(30), &options)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(http_get(absent.url.expose()).await.status, 404);

    // A one second URL stops working.
    let short = store
        .presign_get(
            &key("presigned"),
            Duration::ZERO,
            &PresignOptions::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(http_get(short.url.expose()).await.status, 200);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        http_get(short.url.expose()).await.status,
        403,
        "an expired URL must be refused"
    );

    // Off by default: no URL, and the node serves the bytes.
    let plain = S3BlobStore::new(
        "plain",
        &s3_config(endpoint, &json!({})),
        Some(&credentials()),
    )
    .unwrap();
    assert!(
        plain
            .presign_get(&key("presigned"), Duration::from_secs(30), &options)
            .await
            .unwrap()
            .is_none()
    );
    store.delete(&key("presigned")).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s3_minio_suite() {
    let required = match gate() {
        Gate::Skip(reason) => {
            eprintln!("skipping MinIO tests: {reason}");
            return;
        }
        Gate::Required => true,
        Gate::Probe if docker_answers() => false,
        Gate::Probe => {
            eprintln!(
                "skipping MinIO tests: Docker is not available (set RUSTRYX_IT=1 to require it)"
            );
            return;
        }
    };
    let container = match start_minio().await {
        Ok(container) => container,
        Err(err) if !required => {
            eprintln!("skipping MinIO tests: could not start a container ({err})");
            return;
        }
        Err(err) => panic!("RUSTRYX_IT=1 but MinIO could not be started: {err}"),
    };
    let host = container.get_host().await.unwrap();
    let mapped = container.get_host_port_ipv4(9000.tcp()).await.unwrap();
    let endpoint = format!("http://{host}:{mapped}");

    let config = s3_config(
        &endpoint,
        &json!({"part_size_mib": 5, "max_concurrent_uploads": 4}),
    );
    let store = S3BlobStore::new("minio", &config, Some(&credentials())).unwrap();
    let part_bytes = 5 * MIB;
    wait_until_reachable(&store).await;

    small_object_round_trip(&store).await;
    multipart_round_trip(&store, part_bytes).await;
    must_not_make_the_object_visible_before_commit(&store, part_bytes).await;
    must_not_leave_an_object_after_abort_or_drop(&store, part_bytes).await;
    delete_and_missing(&store).await;
    upload_slots_follow_the_writer_lifecycle(&endpoint).await;
    registry_builds_a_working_store(&endpoint).await;
    wrong_credentials_are_reported_not_hidden(&endpoint).await;
    list_keys_walks_the_bucket_and_skips_foreign_objects(&store, &endpoint).await;
    presigned_downloads_work_without_credentials_and_expire(&endpoint).await;

    // The default 16 MiB part size is what production runs; measure memory with it.
    let production = S3BlobStore::new(
        "production",
        &s3_config(&endpoint, &json!({})),
        Some(&credentials()),
    )
    .unwrap();
    memory_stays_bounded(&production).await;

    container.rm().await.unwrap();
}
