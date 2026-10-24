//! The raw format driven by the real `curl`, through the in-process HTTP harness.
//!
//! Gated: these run only with `RUSTRYX_IT=1` and print `SKIP` otherwise (`curl` must also be on
//! `PATH`). They cover what a build script does with a raw repository: `--upload-file`, plain
//! and ranged and conditional downloads, deletes, listings, checksum side files, and request
//! targets that `curl --path-as-is` sends exactly as written.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::assert_is_empty,
    clippy::duration_suboptimal_units,
    reason = "test scenarios read best as straight-line code"
)]

mod common;

use std::time::Duration;

use common::{raw_builder, raw_server};
use rustryx_core::model::RepoFormat;
use rustryx_testserver::{NewTestRepo, Output, TempProject, UpstreamStub, it_enabled, run_program};
use sha2::{Digest, Sha256};

/// True when this run may use `curl`; prints the reason otherwise.
async fn curl_ready() -> bool {
    if !it_enabled() {
        println!("SKIP: curl: real-client tests are off (set RUSTRYX_IT=1 to run them)");
        return false;
    }
    let probe = TempProject::new();
    let output = run_program(
        "curl",
        &["--version"],
        &[],
        probe.path(),
        Duration::from_secs(20),
    )
    .await;
    if !output.success() {
        println!("SKIP: curl: `curl` cannot run on this machine");
        return false;
    }
    true
}

async fn curl(project: &TempProject, args: &[&str]) -> Output {
    let mut full = vec!["-sS", "--noproxy", "*", "--max-time", "60"];
    full.extend_from_slice(args);
    run_program("curl", &full, &[], project.path(), Duration::from_secs(90)).await
}

/// The status code of a request whose body goes to a file.
async fn status(project: &TempProject, args: &[&str]) -> String {
    let mut full = vec!["-o", "response.bin", "-w", "%{http_code}"];
    full.extend_from_slice(args);
    let output = curl(project, &full).await;
    assert!(output.code == Some(0), "{}", output.combined());
    output.stdout
}

#[tokio::test(flavor = "multi_thread")]
async fn curl_uploads_downloads_and_deletes_files() {
    if !curl_ready().await {
        return;
    }
    let server = raw_server().await;
    let admin = server.add_user("publisher", &["nx-admin"]).await;
    let project = TempProject::new();
    let credentials = format!("{}:{}", admin.username, admin.password);
    let base = server.repository_url("files");
    let content: Vec<u8> = (0..300_000u32)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    project.write("artifact.bin", &content).await;
    let url = format!("{base}/releases/1.0/artifact.bin");

    // Upload with --upload-file (a PUT with Content-Length), twice: created, then replaced.
    assert_eq!(
        status(
            &project,
            &["-u", &credentials, "--upload-file", "artifact.bin", &url]
        )
        .await,
        "201"
    );
    assert_eq!(
        status(
            &project,
            &["-u", &credentials, "--upload-file", "artifact.bin", &url]
        )
        .await,
        "200"
    );

    // Plain download, byte for byte.
    curl(
        &project,
        &["-u", &credentials, "-o", "downloaded.bin", "--fail", &url],
    )
    .await
    .assert_success();
    assert_eq!(project.read("downloaded.bin").await.unwrap(), content);

    // HEAD: the validators and the checksum headers.
    let head = curl(&project, &["-u", &credentials, "-I", &url]).await;
    head.assert_success();
    let headers = head.stdout.to_ascii_lowercase();
    assert!(headers.starts_with("http/1.1 200"), "{headers}");
    for expected in [
        "etag:",
        "last-modified:",
        "accept-ranges: bytes",
        "x-checksum-sha256:",
        "x-content-type-options: nosniff",
        "content-length: 300000",
    ] {
        assert!(headers.contains(expected), "{expected} in {headers}");
    }
    let etag = headers
        .lines()
        .find_map(|line| line.strip_prefix("etag: "))
        .unwrap()
        .trim()
        .to_owned();
    let last_modified = head
        .stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("last-modified: ")
                .or_else(|| line.strip_prefix("Last-Modified: "))
        })
        .unwrap()
        .trim()
        .to_owned();

    // A range.
    assert_eq!(
        status(&project, &["-u", &credentials, "-r", "1000-1009", &url]).await,
        "206"
    );
    assert_eq!(
        project.read("response.bin").await.unwrap(),
        &content[1000..1010]
    );
    assert_eq!(
        status(&project, &["-u", &credentials, "-r", "999999999-", &url]).await,
        "416"
    );

    // Conditional requests.
    let etag_header = format!("If-None-Match: {etag}");
    assert_eq!(
        status(&project, &["-u", &credentials, "-H", &etag_header, &url]).await,
        "304"
    );
    assert_eq!(
        status(&project, &["-u", &credentials, "-z", &last_modified, &url]).await,
        "304"
    );
    assert_eq!(
        status(
            &project,
            &["-u", &credentials, "-H", "If-None-Match: \"other\"", &url]
        )
        .await,
        "200"
    );

    // The computed side file is the digest curl can compare against.
    let expected = hex::encode(Sha256::digest(&content));
    let side = curl(&project, &["-u", &credentials, &format!("{url}.sha256")]).await;
    side.assert_success();
    assert_eq!(side.stdout, expected);

    // Listing: HTML for a person, JSON for a script.
    let html = curl(
        &project,
        &["-u", &credentials, &format!("{base}/releases/1.0/")],
    )
    .await;
    html.assert_success();
    assert!(html.stdout.contains("artifact.bin"), "{}", html.stdout);
    let json = curl(
        &project,
        &[
            "-u",
            &credentials,
            "-H",
            "Accept: application/json",
            &format!("{base}/releases/"),
        ],
    )
    .await;
    json.assert_success();
    let listing: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(listing["items"][0]["name"], "1.0");
    assert_eq!(listing["items"][0]["type"], "directory");

    // Delete, then it is gone.
    assert_eq!(
        status(&project, &["-u", &credentials, "-X", "DELETE", &url]).await,
        "204"
    );
    assert_eq!(status(&project, &["-u", &credentials, &url]).await, "404");
    assert_eq!(
        status(&project, &["-u", &credentials, "-X", "DELETE", &url]).await,
        "404"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn curl_uploads_a_chunked_body_and_meets_the_file_and_directory_rule() {
    if !curl_ready().await {
        return;
    }
    let server = raw_server().await;
    let admin = server.add_user("publisher", &["nx-admin"]).await;
    let project = TempProject::new();
    let credentials = format!("{}:{}", admin.username, admin.password);
    let base = server.repository_url("files");
    project.write("body.txt", b"chunked body").await;

    // No Content-Length: the body travels with Transfer-Encoding: chunked.
    let chunked = format!("{base}/stream/body.txt");
    assert_eq!(
        status(
            &project,
            &[
                "-u",
                &credentials,
                "-X",
                "PUT",
                "-H",
                "Transfer-Encoding: chunked",
                "--data-binary",
                "@body.txt",
                &chunked
            ]
        )
        .await,
        "201"
    );
    let got = curl(&project, &["-u", &credentials, &chunked]).await;
    assert_eq!(got.stdout, "chunked body");

    // `stream` is a directory now, so it cannot be a file; `stream/body.txt` is a file, so
    // nothing can go below it.
    assert_eq!(
        status(
            &project,
            &[
                "-u",
                &credentials,
                "--upload-file",
                "body.txt",
                &format!("{base}/stream")
            ]
        )
        .await,
        "409"
    );
    assert_eq!(
        status(
            &project,
            &[
                "-u",
                &credentials,
                "--upload-file",
                "body.txt",
                &format!("{chunked}/deeper")
            ]
        )
        .await,
        "409"
    );
    // The trailing slash never turns a file into a directory or the other way round.
    assert_eq!(
        status(&project, &["-u", &credentials, &format!("{chunked}/")]).await,
        "404"
    );
    assert_eq!(
        status(&project, &["-u", &credentials, &format!("{base}/stream")]).await,
        "404"
    );
    assert_eq!(
        status(
            &project,
            &[
                "-u",
                &credentials,
                "-X",
                "PUT",
                "-d",
                "x",
                &format!("{base}/stream/")
            ]
        )
        .await,
        "405"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn curl_gets_401_without_credentials_and_cannot_use_a_traversal() {
    if !curl_ready().await {
        return;
    }
    let server = raw_server().await;
    let admin = server.add_user("publisher", &["nx-admin"]).await;
    let project = TempProject::new();
    let credentials = format!("{}:{}", admin.username, admin.password);
    let base = server.repository_url("files");
    project.write("x.txt", b"x").await;
    assert_eq!(
        status(
            &project,
            &[
                "-u",
                &credentials,
                "--upload-file",
                "x.txt",
                &format!("{base}/safe.txt")
            ]
        )
        .await,
        "201"
    );

    assert_eq!(
        status(&project, &[&format!("{base}/safe.txt")]).await,
        "401"
    );
    let challenge = curl(&project, &["-I", &format!("{base}/safe.txt")]).await;
    assert!(
        challenge
            .stdout
            .to_ascii_lowercase()
            .contains("www-authenticate: basic")
    );
    assert_eq!(
        status(
            &project,
            &["-u", "publisher:wrong", &format!("{base}/safe.txt")]
        )
        .await,
        "401"
    );

    // `--path-as-is` keeps `..` and the encoded forms in the request line.
    for suffix in [
        "../etc/passwd",
        "a/../b",
        "x/../safe.txt",
        "%2e%2e/x",
        "%2E%2E%2Fx",
        "a%2f..%2fb",
        "..%5cx",
        "%252e%252e/x",
        "a%00b",
        "a//b",
    ] {
        let url = format!("{base}/{suffix}");
        let code = status(&project, &["--path-as-is", "-u", &credentials, &url]).await;
        assert_eq!(code, "400", "{suffix}");
    }
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn curl_sees_html_as_an_attachment() {
    if !curl_ready().await {
        return;
    }
    let server = raw_server().await;
    let admin = server.add_user("publisher", &["nx-admin"]).await;
    let project = TempProject::new();
    let credentials = format!("{}:{}", admin.username, admin.password);
    let url = format!("{}/site/index.html", server.repository_url("files"));
    project
        .write("index.html", b"<script>alert(1)</script>")
        .await;
    assert_eq!(
        status(
            &project,
            &[
                "-u",
                &credentials,
                "-H",
                "Content-Type: text/html",
                "--upload-file",
                "index.html",
                &url
            ]
        )
        .await,
        "201"
    );
    let head = curl(&project, &["-u", &credentials, "-I", &url]).await;
    let headers = head.stdout.to_ascii_lowercase();
    assert!(headers.contains("content-type: text/html"), "{headers}");
    assert!(
        headers.contains("content-disposition: attachment;"),
        "{headers}"
    );
    assert!(headers.contains("x-content-type-options: nosniff"));
    assert!(headers.contains("content-security-policy: sandbox"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn curl_reads_through_a_proxy_and_a_group() {
    if !curl_ready().await {
        return;
    }
    let upstream = UpstreamStub::start().await;
    upstream
        .serve_file(
            "/tools/cli-2.0.tgz",
            b"upstream archive".to_vec(),
            "application/gzip",
        )
        .await;
    let server = raw_builder().start().await;
    server
        .add_repository(NewTestRepo::hosted("local", RepoFormat::Raw))
        .await;
    server
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Raw,
            &upstream.base_url(),
        ))
        .await;
    server
        .add_repository(NewTestRepo::group(
            "all",
            RepoFormat::Raw,
            &["local", "mirror"],
        ))
        .await;
    let admin = server.add_user("publisher", &["nx-admin"]).await;
    let project = TempProject::new();
    let credentials = format!("{}:{}", admin.username, admin.password);
    project.write("own.txt", b"own file").await;
    assert_eq!(
        status(
            &project,
            &[
                "-u",
                &credentials,
                "--upload-file",
                "own.txt",
                &format!("{}/tools/own.txt", server.repository_url("local"))
            ]
        )
        .await,
        "201"
    );

    // The proxy fetches once and serves the cache afterwards.
    let mirror = format!("{}/tools/cli-2.0.tgz", server.repository_url("mirror"));
    for _ in 0..2 {
        let got = curl(&project, &["-u", &credentials, &mirror]).await;
        got.assert_success();
        assert_eq!(got.stdout, "upstream archive");
        server.settle().await;
    }
    assert_eq!(upstream.count("GET", "/tools/cli-2.0.tgz").await, 1);
    assert_eq!(
        status(
            &project,
            &["-u", &credentials, "-X", "PUT", "-d", "x", &mirror]
        )
        .await,
        "405"
    );

    // The group answers from the member that has the file.
    let group = server.repository_url("all");
    let own = curl(
        &project,
        &["-u", &credentials, &format!("{group}/tools/own.txt")],
    )
    .await;
    assert_eq!(own.stdout, "own file");
    let remote = curl(
        &project,
        &["-u", &credentials, &format!("{group}/tools/cli-2.0.tgz")],
    )
    .await;
    assert_eq!(remote.stdout, "upstream archive");
    assert_eq!(
        status(
            &project,
            &["-u", &credentials, &format!("{group}/tools/none.txt")]
        )
        .await,
        "404"
    );
    server.shutdown().await;
}
