//! Memory behaviour: a large synthetic body streams through hosted upload, hosted
//! download, a proxy cache fill (tee) and a proxy cache hit without ever being held
//! in memory. The blob store used here keeps only a digest and the size of what it
//! is given and regenerates the same synthetic bytes on read, so any growth of the
//! process comes from the code under test, not from the store.
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

use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum_core::body::Body;
use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use rustryx_core::error::BlobError;
use rustryx_core::model::{BlobStoreKind, Checksums, RepoFormat, RepoType};
use rustryx_core::ports::{
    BlobCommit, BlobKey, BlobMeta, BlobRead, BlobStore, BlobWriter, ByteRange,
};
use rustryx_format_core::proxy::ssrf::UpstreamUrlPolicy;
use rustryx_format_core::proxy::upstream::{
    UpstreamClient, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use rustryx_format_core::testing::{synthetic_range, synthetic_stream};
use rustryx_format_core::{FormatEngine, FormatRegistry, Ports};
use rustryx_testkit::{FixedClock, MemAuditSink, MemStore, fixtures};
use sha2::{Digest, Sha256};
use support::{FakeRbac, RawLikeHandler, SingleStore, principal};
use time::OffsetDateTime;

const CHUNK: usize = 64 * 1024;

fn total_bytes() -> u64 {
    let mib: u64 = std::env::var("RUSTRYX_MEMTEST_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    mib * 1024 * 1024
}

/// Resident set size of this process in KiB, from `ps` (works on Linux and macOS).
fn rss_kib() -> u64 {
    let pid = std::process::id().to_string();
    Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// Samples the RSS from a plain thread, so a busy runtime cannot delay the sampling.
struct PeakSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PeakSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(rss_kib()));
        let handle = {
            let (stop, peak) = (Arc::clone(&stop), Arc::clone(&peak));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    peak.fetch_max(rss_kib(), Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        };
        Self {
            stop,
            peak,
            handle: Some(handle),
        }
    }

    fn peak_kib(&self) -> u64 {
        self.peak.load(Ordering::SeqCst)
    }

    fn stop(mut self) -> u64 {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.peak.fetch_max(rss_kib(), Ordering::SeqCst);
        self.peak_kib()
    }
}

/// Blob store that keeps `(size, sha256)` and regenerates the synthetic body on read.
#[derive(Default)]
struct DiscardingStore {
    objects: Mutex<HashMap<String, (u64, String)>>,
}

struct DiscardingWriter {
    store: Arc<DiscardingStore>,
    key: String,
    hasher: Sha256,
    size: u64,
}

#[async_trait]
impl BlobWriter for DiscardingWriter {
    async fn write(&mut self, chunk: Bytes) -> Result<(), BlobError> {
        self.hasher.update(&chunk);
        self.size += chunk.len() as u64;
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<BlobCommit, BlobError> {
        let sha256 = hex::encode(self.hasher.finalize());
        self.store
            .objects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(self.key.clone(), (self.size, sha256.clone()));
        Ok(BlobCommit {
            size_bytes: self.size,
            checksums: Checksums {
                sha1: "0".repeat(40),
                sha256,
                md5: "0".repeat(32),
            },
        })
    }

    async fn abort(self: Box<Self>) -> Result<(), BlobError> {
        Ok(())
    }
}

struct DiscardingHandle(Arc<DiscardingStore>);

#[async_trait]
impl BlobStore for DiscardingHandle {
    fn kind(&self) -> BlobStoreKind {
        BlobStoreKind::Local
    }

    async fn begin(
        &self,
        key: &BlobKey,
        _hint: Option<u64>,
    ) -> Result<Box<dyn BlobWriter>, BlobError> {
        Ok(Box::new(DiscardingWriter {
            store: Arc::clone(&self.0),
            key: key.as_str().to_owned(),
            hasher: Sha256::new(),
            size: 0,
        }))
    }

    async fn get(&self, key: &BlobKey, range: Option<ByteRange>) -> Result<BlobRead, BlobError> {
        let size = self
            .0
            .objects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key.as_str())
            .map(|(size, _)| *size)
            .ok_or(BlobError::NotFound)?;
        let (start, end) = match range {
            None => (0, size),
            Some(r) => (r.start, r.end.map_or(size, |e| (e + 1).min(size))),
        };
        Ok(BlobRead {
            total_size: size,
            stream: synthetic_range(start, end, CHUNK)
                .map(|item| item.map_err(|e| BlobError::Unavailable(e.to_string())))
                .boxed(),
        })
    }

    async fn head(&self, key: &BlobKey) -> Result<Option<BlobMeta>, BlobError> {
        Ok(self
            .0
            .objects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key.as_str())
            .map(|(size, _)| BlobMeta { size_bytes: *size }))
    }

    async fn delete(&self, key: &BlobKey) -> Result<(), BlobError> {
        self.0
            .objects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key.as_str());
        Ok(())
    }

    async fn list_stale_staging(&self, _min_age: Duration) -> Result<Vec<BlobKey>, BlobError> {
        Ok(Vec::new())
    }

    async fn health(&self) -> Result<(), BlobError> {
        Ok(())
    }
}

/// An upstream that serves the synthetic body for every path, streaming.
struct SyntheticUpstream {
    total: u64,
}

#[async_trait]
impl UpstreamClient for SyntheticUpstream {
    async fn send(&self, request: UpstreamRequest) -> Result<UpstreamResponse, UpstreamError> {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(self.total));
        let body = if request.method == Method::HEAD {
            futures::stream::empty().boxed()
        } else {
            synthetic_stream(self.total, CHUNK)
                .map(|item| item.map_err(|e| UpstreamError::Body(e.to_string())))
                .boxed()
        };
        Ok(UpstreamResponse {
            status: StatusCode::OK,
            headers,
            body,
        })
    }
}

/// Reads a response body to the end without keeping it; returns size and SHA-256.
async fn drain(body: Body) -> (u64, String) {
    let mut stream = body.into_data_stream();
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        size += chunk.len() as u64;
        hasher.update(&chunk);
    }
    (size, hex::encode(hasher.finalize()))
}

async fn expected_sha256(total: u64) -> String {
    let mut stream = synthetic_stream(total, CHUNK);
    let mut hasher = Sha256::new();
    while let Some(chunk) = stream.next().await {
        hasher.update(chunk.unwrap());
    }
    hex::encode(hasher.finalize())
}

/// Guards against a vacuous memory test: the sampler must see a real allocation. It runs
/// inside the main test, not beside it: the resident set is per process, so a concurrent
/// allocation in another test thread would be counted against the streaming phases.
fn assert_sampler_sees_allocations() {
    let baseline = rss_kib();
    assert!(baseline > 0, "ps must report the resident set size");
    let sampler = PeakSampler::start();
    let canary = vec![1u8; 96 * 1024 * 1024];
    // Touch every page so the allocation becomes resident.
    let touched: u64 = canary.iter().step_by(4096).map(|b| u64::from(*b)).sum();
    std::thread::sleep(Duration::from_millis(100));
    let peak = sampler.stop();
    assert!(touched > 0);
    assert!(
        peak.saturating_sub(baseline) / 1024 >= 80,
        "a 96 MiB allocation must show up: baseline {baseline} KiB, peak {peak} KiB"
    );
    drop(canary);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn must_not_buffer_whole_artifact_in_memory() {
    assert_sampler_sees_allocations();
    let total = total_bytes();
    let expected = expected_sha256(total).await;

    let clock = Arc::new(FixedClock::new(
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
    ));
    let store = MemStore::with_builtin_roles()
        .with_clock(clock.clone())
        .with_default_blob_store();
    let rbac = Arc::new(FakeRbac::default());
    let discarding = Arc::new(DiscardingStore::default());
    let ports = Ports::from_store(
        Arc::new(store.clone()),
        rbac.clone(),
        Arc::new(MemAuditSink::new()),
        clock,
        Arc::new(SingleStore(Arc::new(DiscardingHandle(Arc::clone(
            &discarding,
        ))))),
    );
    let mut registry = FormatRegistry::new();
    registry.register(Arc::new(RawLikeHandler)).unwrap();
    let engine = FormatEngine::builder(ports, registry)
        .url_policy(UpstreamUrlPolicy::any_public())
        .upstream_client(Arc::new(SyntheticUpstream { total }))
        .build()
        .unwrap();

    let hosted = fixtures::repository("files", RepoFormat::Raw, RepoType::Hosted);
    let proxy = fixtures::repository("mirror", RepoFormat::Raw, RepoType::Proxy);
    rustryx_core::ports::RepositoryRepo::create(&store, &hosted)
        .await
        .unwrap();
    rustryx_core::ports::RepositoryRepo::create(&store, &proxy)
        .await
        .unwrap();
    let (hosted, proxy) = (Arc::new(hosted), Arc::new(proxy));
    rbac.grant_all("alice", "files");
    rbac.grant_all("alice", "mirror");
    let alice = principal("alice");
    let head = |method: Method, path: &str| {
        rustryx_format_core::RequestHead::new(
            method,
            path,
            alice.clone(),
            url::Url::parse("https://rustryx.test").unwrap(),
        )
    };

    // Warm up allocators and connection-independent caches with a small round trip.
    let mut warm = head(Method::PUT, "warm/up.bin");
    warm.headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from_static("1048576"));
    let response = engine
        .serve(
            hosted.clone(),
            warm,
            Body::from_stream(synthetic_stream(1_048_576, CHUNK)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    drain(
        engine
            .serve(
                hosted.clone(),
                head(Method::GET, "warm/up.bin"),
                Body::empty(),
            )
            .await
            .into_body(),
    )
    .await;

    let baseline = rss_kib();
    let sampler = PeakSampler::start();
    let started = Instant::now();

    // 1. Hosted upload, length declared.
    let mut put = head(Method::PUT, "big/artifact.bin");
    put.headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(total));
    let response = engine
        .serve(
            hosted.clone(),
            put,
            Body::from_stream(synthetic_stream(total, CHUNK)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let after_upload = sampler.peak_kib();

    // 2. Hosted download.
    let response = engine
        .serve(
            hosted.clone(),
            head(Method::GET, "big/artifact.bin"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let (size, digest) = drain(response.into_body()).await;
    assert_eq!(
        (size, digest.as_str()),
        (total, expected.as_str()),
        "hosted download content"
    );
    let after_download = sampler.peak_kib();

    // 3. Proxy cache fill: the upstream stream is tee'd to the client and the store.
    let response = engine
        .serve(
            proxy.clone(),
            head(Method::GET, "remote/artifact.bin"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-rustryx-cache").unwrap(), "MISS");
    let (size, digest) = drain(response.into_body()).await;
    assert_eq!(
        (size, digest.as_str()),
        (total, expected.as_str()),
        "proxy tee content"
    );
    let after_fill = sampler.peak_kib();
    for _ in 0..200 {
        if engine.running_tasks() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 4. Proxy cache hit.
    let response = engine
        .serve(
            proxy.clone(),
            head(Method::GET, "remote/artifact.bin"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.headers().get("x-rustryx-cache").unwrap(), "HIT");
    let (size, digest) = drain(response.into_body()).await;
    assert_eq!(
        (size, digest.as_str()),
        (total, expected.as_str()),
        "proxy cache hit content"
    );

    let peak = sampler.stop();
    let growth_mib = peak.saturating_sub(baseline) / 1024;
    let phase = |kib: u64| kib.saturating_sub(baseline) / 1024;
    eprintln!(
        "memory test: {} MiB per phase in {:?}; RSS growth over baseline: upload {} MiB, +download {} MiB, +proxy fill {} MiB, overall peak {} MiB",
        total / 1024 / 1024,
        started.elapsed(),
        phase(after_upload),
        phase(after_download),
        phase(after_fill),
        growth_mib
    );
    assert!(
        growth_mib < 64,
        "RSS grew by {growth_mib} MiB while streaming {} MiB",
        total / 1024 / 1024
    );
    assert_eq!(
        discarding.objects.lock().unwrap().len(),
        3,
        "warm-up, upload and cache fill were stored"
    );
}
