//! The proxy over real HTTP: reqwest client, guarded DNS, redirects, credentials.
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

use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum_core::body::Body;
use gzip_fixture::GZIP_HELLO;
use http::{Method, StatusCode};
use rustryx_core::Secret;
use rustryx_core::model::{Repository, UpstreamAuth};
use rustryx_format_core::proxy::ssrf::{
    HostAllowlist, HostPattern, HostResolver, UpstreamUrlPolicy,
};
use rustryx_format_core::proxy::upstream::ReqwestClientConfig;
use support::{World, body_bytes, body_text, header_of, principal};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A fixed gzip stream (the bytes of "hello"), so no compression crate is needed.
mod gzip_fixture {
    pub const GZIP_HELLO: [u8; 25] = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0x07, 0x00, 0x86, 0xa6, 0x10, 0x36, 0x05, 0x00, 0x00, 0x00,
    ];
}

fn policy(allow_loopback: bool) -> UpstreamUrlPolicy {
    UpstreamUrlPolicy {
        hosts: HostAllowlist::AnyPublic,
        http_hosts: ["127.0.0.1", "localhost", "pinned.test", "rebind.test"]
            .iter()
            .map(|h| HostPattern::parse(h).unwrap())
            .collect(),
        // `localhost` resolves to both families; an answer is accepted only if every address is allowed.
        allow_cidrs: if allow_loopback {
            vec!["127.0.0.0/8".parse().unwrap(), "::1/128".parse().unwrap()]
        } else {
            Vec::new()
        },
        ports: None,
        max_redirects: 3,
    }
}

async fn world_for(server: &MockServer, allow_loopback: bool) -> (World, Arc<Repository>) {
    let world = World::real_http(policy(allow_loopback), ReqwestClientConfig::default());
    let repo = world
        .proxy_at(
            "mirror",
            &format!("http://127.0.0.1:{}/", server.address().port()),
        )
        .await;
    world.allow_all("alice", &[&repo]);
    (world, repo)
}

async fn count(server: &MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

#[tokio::test]
async fn fetches_over_real_http_and_serves_the_second_request_from_the_cache() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/a/b.bin"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(&b"real bytes"[..])
                .insert_header("content-type", "application/java-archive"),
        )
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let alice = principal("alice");
    let response = world.get(&repo, "a/b.bin", &alice).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "content-type"),
        Some("application/java-archive")
    );
    assert_eq!(&body_bytes(response).await[..], b"real bytes");
    world.settle().await;
    let response = world.get(&repo, "a/b.bin", &alice).await;
    assert_eq!(
        header_of(response.headers(), "x-rustryx-cache"),
        Some("HIT")
    );
    assert_eq!(count(&server).await, 1);
}

#[tokio::test]
async fn thirty_concurrent_clients_cause_one_real_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/burst.bin"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![4u8; 20_000])
                .set_delay(Duration::from_millis(150)),
        )
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let alice = principal("alice");
    let mut tasks = Vec::new();
    for _ in 0..30 {
        let engine = world.engine.clone();
        let repo = repo.clone();
        let head = world.head(Method::GET, "burst.bin", &alice);
        tasks.push(tokio::spawn(async move {
            let response = engine.serve(repo, head, Body::empty()).await;
            (response.status(), body_bytes(response).await.len())
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), (StatusCode::OK, 20_000));
    }
    assert_eq!(count(&server).await, 1, "must_not_fetch_upstream_n_times");
}

#[tokio::test]
async fn retries_a_flaky_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/flaky.bin"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/flaky.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"finally"[..]))
        .with_priority(2)
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let response = world.get(&repo, "flaky.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], b"finally");
    assert_eq!(count(&server).await, 3);
}

#[tokio::test]
async fn must_not_decompress_what_the_upstream_sends() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gz.bin"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-encoding", "gzip")
                .set_body_bytes(&GZIP_HELLO[..]),
        )
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let response = world.get(&repo, "gz.bin", &principal("alice")).await;
    assert_eq!(
        &body_bytes(response).await[..],
        &GZIP_HELLO[..],
        "the stored bytes must be the bytes the checksums describe"
    );
}

#[tokio::test]
async fn sends_repository_credentials_to_the_upstream() {
    let server = MockServer::start().await;
    use base64::Engine as _;
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("svc:s3cret")
    );
    Mock::given(method("GET"))
        .and(path("/secured.bin"))
        .and(header("authorization", expected.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"ok"[..]))
        .mount(&server)
        .await;
    let world = World::real_http(policy(true), ReqwestClientConfig::default());
    let mut repo = rustryx_testkit::fixtures::repository(
        "secured",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Proxy,
    );
    {
        let proxy = repo.proxy.as_mut().unwrap();
        proxy.remote_url = format!("http://127.0.0.1:{}/", server.address().port());
        proxy.auth = Some(UpstreamAuth {
            username: "svc".into(),
            password: Secret::new("s3cret"),
        });
    }
    let repo = world.add(repo).await;
    world.allow_all("alice", &[&repo]);
    assert_eq!(
        world
            .get(&repo, "secured.bin", &principal("alice"))
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn must_not_connect_to_loopback_unless_the_policy_allows_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"secret internal"[..]))
        .mount(&server)
        .await;
    // Literal address.
    let (world, repo) = world_for(&server, false).await;
    let response = world.get(&repo, "x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(body_text(response).await, "RAW-ERROR upstream_blocked");
    // A host name that resolves to loopback is caught by the guarded resolver at connect time.
    let by_name = world
        .proxy_at(
            "by-name",
            &format!("http://localhost:{}/", server.address().port()),
        )
        .await;
    world.allow_all("alice", &[&by_name]);
    let response = world.get(&by_name, "x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(body_text(response).await, "RAW-ERROR upstream_blocked");
    assert_eq!(
        count(&server).await,
        0,
        "the connection must never have been opened"
    );
}

#[tokio::test]
async fn an_explicit_cidr_allowlist_opens_a_named_internal_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"internal"[..]))
        .mount(&server)
        .await;
    let world = World::real_http(policy(true), ReqwestClientConfig::default());
    let repo = world
        .proxy_at(
            "by-name",
            &format!("http://localhost:{}/", server.address().port()),
        )
        .await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], b"internal");
    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received[0].headers.get("host").unwrap().to_str().unwrap(),
        format!("localhost:{}", server.address().port()),
        "the Host header keeps the original name"
    );
}

struct CountingResolver {
    lookups: AtomicUsize,
    answers: Vec<IpAddr>,
}

#[async_trait]
impl HostResolver for CountingResolver {
    async fn lookup(&self, _host: &str) -> io::Result<Vec<IpAddr>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        Ok(self.answers.clone())
    }
}

#[tokio::test]
async fn resolves_once_and_connects_to_the_validated_address() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pin.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"pinned"[..]))
        .mount(&server)
        .await;
    let resolver = Arc::new(CountingResolver {
        lookups: AtomicUsize::new(0),
        answers: vec!["127.0.0.1".parse().unwrap()],
    });
    let config = ReqwestClientConfig {
        resolver: resolver.clone(),
        ..ReqwestClientConfig::default()
    };
    let world = World::real_http(policy(true), config);
    let repo = world
        .proxy_at(
            "pinned",
            &format!("http://pinned.test:{}/", server.address().port()),
        )
        .await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "pin.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&body_bytes(response).await[..], b"pinned");
    assert_eq!(
        resolver.lookups.load(Ordering::SeqCst),
        1,
        "one lookup per connection: no second resolution between check and connect"
    );
    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received[0].headers.get("host").unwrap().to_str().unwrap(),
        format!("pinned.test:{}", server.address().port())
    );
}

#[tokio::test]
async fn must_not_connect_when_a_dns_answer_mixes_public_and_internal_addresses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let resolver = Arc::new(CountingResolver {
        lookups: AtomicUsize::new(0),
        answers: vec![
            "93.184.216.34".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        ],
    });
    let config = ReqwestClientConfig {
        resolver,
        ..ReqwestClientConfig::default()
    };
    // Loopback is NOT allowlisted here: the rebinding style answer must be refused whole.
    let world = World::real_http(policy(false), config);
    let repo = world
        .proxy_at(
            "rebind",
            &format!("http://rebind.test:{}/", server.address().port()),
        )
        .await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(count(&server).await, 0);
}

#[tokio::test]
async fn follows_same_origin_redirects_and_keeps_credentials_there() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/old.bin"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/new.bin"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/new.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"moved"[..]))
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let response = world.get(&repo, "old.bin", &principal("alice")).await;
    assert_eq!(&body_bytes(response).await[..], b"moved");
    assert_eq!(count(&server).await, 2);
}

#[tokio::test]
async fn must_not_send_credentials_to_another_origin_after_a_redirect() {
    let origin = MockServer::start().await;
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hop.bin"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            format!("http://127.0.0.1:{}/data.bin", other.address().port()),
        ))
        .mount(&origin)
        .await;
    Mock::given(method("GET"))
        .and(path("/data.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"cdn bytes"[..]))
        .mount(&other)
        .await;
    let world = World::real_http(policy(true), ReqwestClientConfig::default());
    let mut repo = rustryx_testkit::fixtures::repository(
        "hop",
        rustryx_core::model::RepoFormat::Raw,
        rustryx_core::model::RepoType::Proxy,
    );
    {
        let proxy = repo.proxy.as_mut().unwrap();
        proxy.remote_url = format!("http://127.0.0.1:{}/", origin.address().port());
        proxy.auth = Some(UpstreamAuth {
            username: "svc".into(),
            password: Secret::new("s3cret"),
        });
    }
    let repo = world.add(repo).await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "hop.bin", &principal("alice")).await;
    assert_eq!(&body_bytes(response).await[..], b"cdn bytes");
    let first = origin.received_requests().await.unwrap();
    assert!(
        first[0].headers.contains_key("authorization"),
        "the origin gets the credentials"
    );
    let second = other.received_requests().await.unwrap();
    assert!(
        !second[0].headers.contains_key("authorization"),
        "another origin must not"
    );
}

#[tokio::test]
async fn must_not_follow_a_redirect_to_the_metadata_service() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/evil.bin"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
        )
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let response = world.get(&repo, "evil.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(body_text(response).await, "RAW-ERROR upstream_blocked");
}

#[tokio::test]
async fn a_redirect_loop_ends_after_three_hops() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loop.bin"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop.bin"))
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let response = world.get(&repo, "loop.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    // 4 requests per attempt (the first plus three hops); redirect failures are not retried.
    assert_eq!(count(&server).await, 4);
}

#[tokio::test]
async fn a_slow_upstream_head_times_out_with_504() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow.bin"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(900)))
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let response = world.get(&repo, "slow.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn a_refused_connection_is_a_502() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let world = World::real_http(policy(true), ReqwestClientConfig::default());
    let repo = world
        .proxy_at("closed", &format!("http://127.0.0.1:{port}/"))
        .await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "x.bin", &principal("alice")).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(body_text(response).await, "RAW-ERROR upstream_unreachable");
}

#[tokio::test]
async fn head_requests_use_the_head_method_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/h.bin"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "1234"))
        .mount(&server)
        .await;
    let (world, repo) = world_for(&server, true).await;
    let response = world
        .send(
            &repo,
            Method::HEAD,
            "h.bin",
            &principal("alice"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(response.headers(), "content-length"),
        Some("1234")
    );
}

/// A server that announces `announced` bytes, sends `sent` of them and closes.
async fn truncating_server(announced: usize, sent: usize) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {announced}\r\nContent-Type: application/octet-stream\r\n\r\n"
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&vec![b'z'; sent]).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test]
async fn a_connection_dropped_mid_body_breaks_the_client_stream_and_caches_nothing() {
    let port = truncating_server(10_000, 1_000).await;
    let world = World::real_http(policy(true), ReqwestClientConfig::default());
    let repo = world
        .proxy_at("cut", &format!("http://127.0.0.1:{port}/"))
        .await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "cut.bin", &principal("alice")).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the head was already sent when the upstream broke"
    );
    let result = rustryx_format_core::stream::read_bounded(response.into_body(), 100_000).await;
    assert!(
        result.is_err(),
        "a truncated download must not end like a complete one"
    );
    world.settle().await;
    assert_eq!(world.blobs.len(), 0);
    assert_eq!(world.blobs.staging_count(), 0);
}

#[tokio::test]
async fn a_complete_body_from_a_raw_server_is_cached() {
    let port = truncating_server(1_000, 1_000).await;
    let world = World::real_http(policy(true), ReqwestClientConfig::default());
    let repo = world
        .proxy_at("whole", &format!("http://127.0.0.1:{port}/"))
        .await;
    world.allow_all("alice", &[&repo]);
    let response = world.get(&repo, "whole.bin", &principal("alice")).await;
    assert_eq!(body_bytes(response).await.len(), 1_000);
    world.settle().await;
    assert_eq!(world.blobs.len(), 1);
}
