//! The upstream stub, and a proxy over it for the shapes of failure a real upstream has.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]
#![allow(
    clippy::too_many_lines,
    reason = "test scenarios read best as straight-line code"
)]

mod common;

use std::time::{Duration, Instant};

use common::{client, get, header_text, raw_builder};
use reqwest::StatusCode;
use rustryx_core::model::RepoFormat;
use rustryx_testserver::{NewTestRepo, UpstreamStub, fast_executor_config};

#[tokio::test(flavor = "multi_thread")]
async fn the_stub_serves_counts_and_resets() {
    let stub = UpstreamStub::start().await;
    stub.serve_file("/a.txt", b"alpha".to_vec(), "text/plain")
        .await;
    stub.status("/boom", 500).await;
    stub.slow("/slow", b"s".to_vec(), Duration::from_millis(50))
        .await;
    let http = client();

    let response = http
        .get(format!("{}/a.txt?x=1", stub.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_text(&response, "content-type").as_deref(),
        Some("text/plain")
    );
    assert_eq!(response.text().await.unwrap(), "alpha");
    let head = http
        .head(format!("{}/a.txt", stub.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(header_text(&head, "content-length").as_deref(), Some("5"));
    assert_eq!(
        http.get(format!("{}/boom", stub.url()))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        http.get(format!("{}/nothing", stub.url()))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let started = Instant::now();
    http.get(format!("{}/slow", stub.url()))
        .send()
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(50));

    assert_eq!(stub.count("GET", "/a.txt").await, 1);
    assert_eq!(stub.count("HEAD", "/a.txt").await, 1);
    assert_eq!(stub.request_count().await, 5);
    let calls = stub.requests().await;
    assert_eq!(calls[0].query.as_deref(), Some("x=1"));
    assert!(calls[0].header("user-agent").is_none() || calls[0].header("host").is_some());
    assert!(stub.base_url().ends_with('/'));
    assert!(stub.server().address().port() > 0);

    stub.reset().await;
    assert_eq!(stub.request_count().await, 0);
    assert_eq!(
        http.get(format!("{}/a.txt", stub.url()))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND,
        "reset forgets the scripted answers"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stub_redirects() {
    let stub = UpstreamStub::start().await;
    stub.serve_file("/target.txt", b"landed".to_vec(), "text/plain")
        .await;
    stub.redirect("/old.txt", 302, "/target.txt").await;
    let response = client()
        .get(format!("{}/old.txt", stub.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "landed");
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_cache_or_serve_as_complete_a_truncated_upstream_body() {
    let stub = UpstreamStub::start().await;
    stub.truncated("/cut.bin", vec![7_u8; 100], 1000).await;
    let server = raw_builder()
        .executor_config(fast_executor_config())
        .start()
        .await;
    server
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Raw,
            &stub.base_url(),
        ))
        .await;
    let user = server.add_user("alice", &["nx-developer"]).await;
    let url = format!("{}/cut.bin", server.repository_url("mirror"));

    // The client either sees an error status or a stream that breaks before 1000 bytes; it must
    // never see a clean 1000 byte answer.
    let outcome = get(&client(), &url, Some(&user.bearer_header())).await;
    let complete = if outcome.status() == StatusCode::OK {
        outcome.bytes().await.is_ok_and(|bytes| bytes.len() == 1000)
    } else {
        false
    };
    assert!(!complete, "a truncated body must not look complete");
    server.settle().await;
    assert_eq!(server.blobs().len(), 0, "nothing may be cached");
    server.shutdown().await;
}
