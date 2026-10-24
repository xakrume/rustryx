//! The format behind a real HTTP listener (the harness server) without any package client:
//! what travels on the wire, raw request lines, concurrency against a real upstream.
#![allow(
    clippy::unwrap_used,
    clippy::unused_self,
    clippy::format_collect,
    clippy::duration_suboptimal_units,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::large_futures,
    clippy::format_push_string,
    clippy::case_sensitive_file_extension_comparisons,
    reason = "tests: a panic is the failure signal and scenarios read best as straight-line code"
)]

use std::sync::Arc;
use std::time::Duration;

use rustryx_core::model::{RepoFormat, WritePolicy};
use rustryx_format_maven::MavenHandler;
use rustryx_testserver::{
    NewTestRepo, READ, TestPrivilege, TestServer, TestUser, UpstreamStub, loopback_friendly_policy,
};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const JAR: &str = "org/acme/lib/1.0/lib-1.0.jar";

async fn start() -> TestServer {
    TestServer::builder()
        .register(Arc::new(MavenHandler::new()))
        .upstream_policy(loopback_friendly_policy())
        .anonymous_enabled(true)
        .start()
        .await
}

fn sha1_hex(data: &[u8]) -> String {
    hex::encode(Sha1::digest(data))
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

async fn put(url: &str, user: &TestUser, body: impl Into<reqwest::Body>) -> reqwest::Response {
    http()
        .put(url)
        .basic_auth(&user.username, Some(&user.token))
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn get(url: &str, user: &TestUser) -> reqwest::Response {
    http()
        .get(url)
        .basic_auth(&user.username, Some(&user.token))
        .send()
        .await
        .unwrap()
}

/// Sends the request bytes as they are (no client normalizes the path) and returns the status
/// line and the body text.
async fn raw(server: &TestServer, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buffer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap();
    (status, text)
}

#[tokio::test(flavor = "multi_thread")]
async fn deploy_and_download_with_credentials_over_http() {
    let server = start().await;
    server
        .add_repository(NewTestRepo::hosted("hosted", RepoFormat::Maven2))
        .await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let base = server.repository_url("hosted");

    let created = put(&format!("{base}/{JAR}"), &dev, "jar bytes").await;
    assert_eq!(created.status(), 201);
    let sha = put(&format!("{base}/{JAR}.sha1"), &dev, sha1_hex(b"jar bytes")).await;
    assert_eq!(sha.status(), 201);
    let bad = put(&format!("{base}/{JAR}.md5"), &dev, "0".repeat(32)).await;
    assert_eq!(bad.status(), 400);

    let download = get(&format!("{base}/{JAR}"), &dev).await;
    assert_eq!(download.status(), 200);
    assert_eq!(download.headers()["content-length"], "9");
    assert_eq!(download.bytes().await.unwrap(), "jar bytes");
    let checksum = get(&format!("{base}/{JAR}.sha1"), &dev)
        .await
        .text()
        .await
        .unwrap();
    assert_eq!(checksum, sha1_hex(b"jar bytes"));
    let metadata = get(&format!("{base}/org/acme/lib/maven-metadata.xml"), &dev)
        .await
        .text()
        .await
        .unwrap();
    assert!(metadata.contains("<release>1.0</release>"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_is_401_with_a_challenge_a_reader_is_403_and_anonymous_read_needs_the_flag() {
    let server = start().await;
    server
        .add_repository(NewTestRepo::hosted("private", RepoFormat::Maven2))
        .await;
    server
        .add_repository(NewTestRepo::hosted("open", RepoFormat::Maven2).allow_anonymous(true))
        .await;
    let role = server
        .add_role_with_privileges("readers", &[TestPrivilege::repository_view("*", &READ)])
        .await;
    let reader = server.add_user("reader", &[role.name.as_str()]).await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    for name in ["private", "open"] {
        put(&format!("{}/{JAR}", server.repository_url(name)), &dev, "x").await;
    }

    let anonymous = http()
        .get(format!("{}/{JAR}", server.repository_url("private")))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    assert!(
        anonymous.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Basic")
    );

    let read = http()
        .get(format!("{}/{JAR}", server.repository_url("open")))
        .send()
        .await
        .unwrap();
    assert_eq!(
        read.status(),
        200,
        "the repository flag and the global switch are both on"
    );
    let write = http()
        .put(format!("{}/{JAR}", server.repository_url("open")))
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(write.status(), 401, "anonymous never writes");

    let forbidden = put(
        &format!(
            "{}/org/acme/lib/2.0/lib-2.0.jar",
            server.repository_url("private")
        ),
        &reader,
        "x",
    )
    .await;
    assert_eq!(forbidden.status(), 403);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_resolve_traversal_in_the_raw_request_line() {
    let server = start().await;
    server
        .add_repository(NewTestRepo::hosted("hosted", RepoFormat::Maven2))
        .await;
    let dev = server.add_user("dev", &["nx-admin"]).await;
    put(
        &format!("{}/{JAR}", server.repository_url("hosted")),
        &dev,
        "x",
    )
    .await;
    let auth = format!("Authorization: Basic {}", {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", dev.username, dev.token))
    });
    for target in [
        "/repository/hosted/../etc/passwd",
        "/repository/hosted/org/../../etc/passwd",
        "/repository/hosted/%2e%2e/%2e%2e/etc/passwd",
        "/repository/hosted/org/%252e%252e/acme/lib/1.0/lib-1.0.jar",
        "/repository/hosted/org%2facme/lib/1.0/lib-1.0.jar",
        "/repository/hosted/org/acme/lib/1.0/..%5c..%5clib-1.0.jar",
        "/repository/hosted/org/acme/lib/1.0/lib-1.0.jar%00.png",
        "/repository/hosted/org//acme/lib/1.0/lib-1.0.jar",
    ] {
        for verb in ["GET", "PUT", "DELETE"] {
            let request = format!(
                "{verb} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let (status, text) = raw(&server, &request).await;
            assert!(
                status == 400 || status == 404,
                "{verb} {target}: {status}\n{text}"
            );
            assert!(!text.contains("root:"), "{target}");
        }
    }
    // The artifact itself is untouched.
    assert_eq!(
        get(&format!("{}/{JAR}", server.repository_url("hosted")), &dev)
            .await
            .status(),
        200
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_metadata_and_xxe_are_refused_on_the_wire() {
    let server = start().await;
    server
        .add_repository(NewTestRepo::hosted("hosted", RepoFormat::Maven2))
        .await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let url = format!(
        "{}/org/acme/lib/maven-metadata.xml",
        server.repository_url("hosted")
    );

    let xxe = "<?xml version=\"1.0\"?><!DOCTYPE m [<!ENTITY x SYSTEM \"file:///etc/passwd\">]>\
               <metadata><groupId>&x;</groupId></metadata>";
    let response = put(&url, &dev, xxe).await;
    assert_eq!(response.status(), 422);
    assert!(!response.text().await.unwrap().contains("root:"));

    // Over the default 10 MiB cap by `Content-Length`: the server answers 413 from the headers
    // alone and never reads the body. Only the head is sent on purpose: a client that streams
    // 11 MiB races the server closing the connection after its answer, which is correct server
    // behaviour and would make this test depend on timing.
    let auth = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", dev.username, dev.token))
    };
    let request = format!(
        "PUT /repository/hosted/org/acme/lib/maven-metadata.xml HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Basic {auth}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        11 * 1024 * 1024
    );
    let (status, text) = raw(&server, &request).await;
    assert_eq!(status, 413, "{text}");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_release_policy_and_allow_once_answer_with_the_documented_statuses() {
    let server = start().await;
    server
        .add_repository(
            NewTestRepo::hosted("releases", RepoFormat::Maven2)
                .write_policy(WritePolicy::AllowOnce)
                .format_config(serde_json::json!({"version_policy": "release"})),
        )
        .await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let base = server.repository_url("releases");
    assert_eq!(
        put(&format!("{base}/{JAR}"), &dev, "one").await.status(),
        201
    );
    assert_eq!(
        put(&format!("{base}/{JAR}"), &dev, "two").await.status(),
        409
    );
    let snapshot = format!("{base}/org/acme/lib/1.0-SNAPSHOT/lib-1.0-20240102.030405-1.jar");
    let refused = put(&snapshot, &dev, "x").await;
    assert_eq!(refused.status(), 400);
    assert!(
        refused
            .text()
            .await
            .unwrap()
            .contains("only release versions")
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_over_http_verifies_the_sidecar_caches_and_deduplicates_concurrent_misses() {
    let server = start().await;
    let upstream = UpstreamStub::start().await;
    server
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Maven2,
            &upstream.base_url(),
        ))
        .await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let base = server.repository_url("mirror");

    let good = vec![7u8; 300_000];
    upstream
        .serve_file(&format!("/{JAR}"), good.clone(), "application/java-archive")
        .await;
    upstream
        .serve_file(&format!("/{JAR}.sha1"), sha1_hex(&good), "text/plain")
        .await;
    let evil = "org/acme/lib/2.0/lib-2.0.jar";
    upstream
        .serve_file(
            &format!("/{evil}"),
            b"poisoned".to_vec(),
            "application/java-archive",
        )
        .await;
    upstream
        .serve_file(&format!("/{evil}.sha1"), sha1_hex(b"genuine"), "text/plain")
        .await;

    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let url = format!("{base}/{JAR}");
            let dev = dev.clone();
            tokio::spawn(async move { get(&url, &dev).await.bytes().await.unwrap().to_vec() })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.unwrap(), good);
    }
    assert_eq!(
        upstream.count("GET", &format!("/{JAR}")).await,
        1,
        "one upstream fetch for 20 clients"
    );

    let response = get(&format!("{base}/{evil}"), &dev).await;
    assert_eq!(response.status(), 502);
    assert!(!response.text().await.unwrap().contains("poisoned"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_over_http_merges_metadata_and_derives_its_checksum() {
    let server = start().await;
    for name in ["team-a", "team-b"] {
        server
            .add_repository(NewTestRepo::hosted(name, RepoFormat::Maven2))
            .await;
    }
    server
        .add_repository(NewTestRepo::group(
            "all",
            RepoFormat::Maven2,
            &["team-a", "team-b"],
        ))
        .await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    put(
        &format!("{}/{JAR}", server.repository_url("team-a")),
        &dev,
        "a",
    )
    .await;
    put(
        &format!(
            "{}/org/acme/lib/1.1/lib-1.1.jar",
            server.repository_url("team-b")
        ),
        &dev,
        "b",
    )
    .await;

    let group = server.repository_url("all");
    let xml = get(&format!("{group}/org/acme/lib/maven-metadata.xml"), &dev)
        .await
        .text()
        .await
        .unwrap();
    assert!(xml.contains("<version>1.0</version>") && xml.contains("<version>1.1</version>"));
    assert!(xml.contains("<release>1.1</release>"));
    let sha = get(
        &format!("{group}/org/acme/lib/maven-metadata.xml.sha1"),
        &dev,
    )
    .await
    .text()
    .await
    .unwrap();
    assert_eq!(sha, sha1_hex(xml.as_bytes()));
    let write = put(&format!("{group}/{JAR}"), &dev, "x").await;
    assert_eq!(write.status(), 405);
    server.shutdown().await;
}
