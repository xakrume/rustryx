//! Streaming through the real HTTP server: a large body must not be held in memory by the
//! harness or the engine, on upload, download, a proxy cache fill (tee) or a cache hit.
//!
//! The blob store keeps only a digest and a size and regenerates the body on read, so any
//! growth of the process comes from the code under test. The resident set is per process:
//! this is the only test of this binary, so nothing else allocates while it measures.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    reason = "one scenario reads best as straight-line code"
)]

mod common;

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use common::{client, header_text, raw_builder};
use futures::StreamExt;
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use reqwest::Body;
use rustryx_core::model::RepoFormat;
use rustryx_format_core::proxy::upstream::{
    UpstreamClient, UpstreamError, UpstreamRequest, UpstreamResponse,
};
use rustryx_testserver::{NewTestRepo, SyntheticBlobStore, synthetic_stream};
use sha2::{Digest, Sha256};

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

/// Samples the resident set from a plain thread, so a busy runtime cannot delay the sampling.
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
async fn drain(response: reqwest::Response) -> (u64, String) {
    let mut stream = response.bytes_stream();
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

/// Guards against a vacuous memory test: the sampler must see a real allocation.
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

fn upload_request(http: &reqwest::Client, url: &str, auth: &str, total: u64) -> reqwest::Request {
    http.put(url)
        .header("authorization", auth)
        .header("content-length", total)
        .body(Body::wrap_stream(synthetic_stream(total, CHUNK)))
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn must_not_buffer_whole_artifact_in_memory() {
    assert_sampler_sees_allocations();
    let total = total_bytes();
    let expected = expected_sha256(total).await;

    let store = SyntheticBlobStore::new();
    let server = raw_builder()
        .blob_store(store.as_blob_store())
        .upstream_client(Arc::new(SyntheticUpstream { total }))
        .start()
        .await;
    server
        .add_repository(NewTestRepo::hosted("files", RepoFormat::Raw))
        .await;
    server
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Raw,
            "https://upstream.example.test/",
        ))
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let auth = alice.bearer_header();
    let http = client();
    let files = server.repository_url("files");
    let mirror = server.repository_url("mirror");

    // Warm up allocators and connection state with a small round trip.
    let warm = upload_request(&http, &format!("{files}/warm/up.bin"), &auth, 1_048_576);
    assert_eq!(
        http.execute(warm).await.unwrap().status(),
        StatusCode::CREATED
    );
    drain(
        http.get(format!("{files}/warm/up.bin"))
            .header("authorization", &auth)
            .send()
            .await
            .unwrap(),
    )
    .await;

    let baseline = rss_kib();
    let sampler = PeakSampler::start();
    let started = Instant::now();

    // 1. Hosted upload, length declared.
    let response = http
        .execute(upload_request(
            &http,
            &format!("{files}/big/artifact.bin"),
            &auth,
            total,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let after_upload = sampler.peak_kib();

    // 2. Hosted download.
    let response = http
        .get(format!("{files}/big/artifact.bin"))
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (size, digest) = drain(response).await;
    assert_eq!(
        (size, digest.as_str()),
        (total, expected.as_str()),
        "hosted download content"
    );
    let after_download = sampler.peak_kib();

    // 3. Proxy cache fill: the upstream stream is tee'd to the client and the store.
    let response = http
        .get(format!("{mirror}/remote/artifact.bin"))
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_text(&response, "x-rustryx-cache").as_deref(),
        Some("MISS")
    );
    let (size, digest) = drain(response).await;
    assert_eq!(
        (size, digest.as_str()),
        (total, expected.as_str()),
        "proxy tee content"
    );
    let after_fill = sampler.peak_kib();
    server.settle().await;

    // 4. Proxy cache hit.
    let response = http
        .get(format!("{mirror}/remote/artifact.bin"))
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(
        header_text(&response, "x-rustryx-cache").as_deref(),
        Some("HIT")
    );
    let (size, digest) = drain(response).await;
    assert_eq!(
        (size, digest.as_str()),
        (total, expected.as_str()),
        "proxy cache hit content"
    );

    let peak = sampler.stop();
    let growth_mib = peak.saturating_sub(baseline) / 1024;
    let phase = |kib: u64| kib.saturating_sub(baseline) / 1024;
    eprintln!(
        "memory test over HTTP: {} MiB per phase in {:?}; RSS growth over baseline: upload {} MiB, +download {} MiB, +proxy fill {} MiB, overall peak {} MiB",
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
        store.object_count(),
        3,
        "warm-up, upload and cache fill were stored"
    );
    server.shutdown().await;
}
