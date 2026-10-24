//! Real package clients against the harness. Gated: they run only with `RUSTRYX_IT=1` and
//! skip (printing `SKIP`) when a tool is missing, so the default `cargo test` stays fast and
//! machine independent.
//!
//! These tests prove the harness, not a format: each uses the raw-like test handler under the
//! label the client expects, and checks what the harness is responsible for (the credential
//! forms, the challenges, the token endpoint, Docker reachability).
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

use std::sync::Arc;

use common::{client, get};
use reqwest::StatusCode;
use rustryx_core::model::RepoFormat;
use rustryx_testserver::{
    NewTestRepo, RawLikeHandler, TempProject, TestServer, Tool, require_tool, run,
};

/// The version command of each tool.
fn version_args(tool: Tool) -> &'static [&'static str] {
    match tool {
        Tool::Mvn => &["-v"],
        Tool::Npm | Tool::Pip | Tool::Twine | Tool::Skopeo => &["--version"],
        Tool::Docker => &["version", "--format", "{{.Client.Version}}"],
        Tool::Helm => &["version", "--short"],
        Tool::DartDocker => &["dart", "--version"],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_available_client_runs() {
    for tool in Tool::ALL {
        if !rustryx_testserver::require(tool).await {
            continue;
        }
        let project = TempProject::new();
        let output = run(tool, version_args(tool), &[], project.path()).await;
        output.assert_success();
        assert!(
            !output.combined().trim().is_empty(),
            "{} printed nothing",
            tool.name()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mvn_deploy_authenticates_with_basic_credentials_and_stores_the_files() {
    require_tool!(Tool::Mvn);
    let server = TestServer::builder()
        .register(Arc::new(RawLikeHandler::for_format(RepoFormat::Maven2)))
        .start()
        .await;
    server
        .add_repository(NewTestRepo::hosted("maven-hosted", RepoFormat::Maven2))
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let project = TempProject::new();
    project
        .write(
            "pom.xml",
            format!(
                r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>org.acme</groupId>
  <artifactId>demo</artifactId>
  <version>1.0</version>
  <packaging>pom</packaging>
  <distributionManagement>
    <repository><id>rustryx</id><url>{}</url></repository>
  </distributionManagement>
</project>"#,
                server.repository_url("maven-hosted")
            ),
        )
        .await;
    // The token in the password position, which is how a CI job carries it.
    project
        .write(
            "settings.xml",
            format!(
                r"<settings><servers><server>
  <id>rustryx</id><username>alice</username><password>{}</password>
</server></servers></settings>",
                alice.token
            ),
        )
        .await;
    let output = run(
        Tool::Mvn,
        &["-B", "-ntp", "-s", "settings.xml", "deploy"],
        &[],
        project.path(),
    )
    .await;
    output.assert_success();

    // Maven sends credentials only after a 401 challenge: the harness answered it, and the
    // pom, its checksums and the metadata are now in the repository.
    let http = client();
    let pom = get(
        &http,
        &format!(
            "{}/org/acme/demo/1.0/demo-1.0.pom",
            server.repository_url("maven-hosted")
        ),
        Some(&alice.basic_header()),
    )
    .await;
    assert_eq!(pom.status(), StatusCode::OK);
    assert!(
        pom.text()
            .await
            .unwrap()
            .contains("<artifactId>demo</artifactId>")
    );
    assert!(server.blobs().len() >= 2, "pom and metadata are stored");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn skopeo_login_follows_the_challenge_to_the_token_endpoint() {
    require_tool!(Tool::Skopeo);
    let server = TestServer::builder()
        .register(Arc::new(RawLikeHandler::for_format(RepoFormat::Docker)))
        .start()
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let project = TempProject::new();
    let registry = format!("127.0.0.1:{}", server.port());
    let auth = project.join("auth.json");
    let auth_arg = auth.to_string_lossy().into_owned();
    let env = project.isolated_env();
    let output = run(
        Tool::Skopeo,
        &[
            "login",
            "--tls-verify=false",
            "--authfile",
            &auth_arg,
            "-u",
            "alice",
            "-p",
            &alice.token,
            &registry,
        ],
        &TempProject::env_pairs(&env),
        project.path(),
    )
    .await;
    output.assert_success();
    let stored = String::from_utf8(project.read("auth.json").await.unwrap()).unwrap();
    assert!(stored.contains(&registry), "{stored}");

    // The wrong password is refused by the token endpoint.
    let wrong = run(
        Tool::Skopeo,
        &[
            "login",
            "--tls-verify=false",
            "--authfile",
            &auth_arg,
            "-u",
            "alice",
            "-p",
            "wrong-password",
            &registry,
        ],
        &TempProject::env_pairs(&env),
        project.path(),
    )
    .await;
    assert!(!wrong.success(), "{}", wrong.combined());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn helm_registry_login_follows_the_token_flow() {
    require_tool!(Tool::Helm);
    let server = TestServer::builder()
        .register(Arc::new(RawLikeHandler::for_format(RepoFormat::Oci)))
        .start()
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let project = TempProject::new();
    let registry = format!("127.0.0.1:{}", server.port());
    let output = run(
        Tool::Helm,
        &[
            "registry",
            "login",
            &registry,
            "--plain-http",
            "-u",
            "alice",
            "-p",
            &alice.token,
        ],
        &[],
        project.path(),
    )
    .await;
    output.assert_success();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dart_container_reaches_the_host_and_pub_sends_its_token() {
    require_tool!(Tool::DartDocker);
    let server = TestServer::builder()
        .bind_all(true)
        .register(Arc::new(RawLikeHandler::for_format(RepoFormat::Pub)))
        .start()
        .await;
    server
        .add_repository(NewTestRepo::hosted("dart", RepoFormat::Pub))
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let project = TempProject::new();
    project
        .write(
            "probe.dart",
            r"import 'dart:convert';
import 'dart:io';
Future<void> main(List<String> args) async {
  final client = HttpClient();
  final response = await (await client.getUrl(Uri.parse(args[0]))).close();
  final body = await response.transform(utf8.decoder).join();
  print('status ${response.statusCode} ${response.headers.value('www-authenticate')}');
  print(body.length);
  client.close(force: true);
}
",
        )
        .await;

    // Both names reach the host from inside the container.
    for base in [
        server.host_base_url(),
        format!("http://localhost:{}", server.port()),
    ] {
        let output = run(
            Tool::DartDocker,
            &[
                "dart",
                "run",
                "probe.dart",
                &format!("{base}/repository/dart/api/packages/demo"),
            ],
            &[],
            project.path(),
        )
        .await;
        output.assert_success();
        // No token: the harness answered 401 with the pub challenge.
        assert!(
            output.stdout.contains(r#"status 401 Bearer realm="pub""#),
            "{}",
            output.combined()
        );
    }

    // pub refuses a token for plain http unless the host is `localhost`; with it, the token
    // reaches the server, so the answer is "no such package" (404), not an authentication error.
    let hosted_url = server.localhost_repository_url("dart");
    project
        .write(
            "pubspec.yaml",
            format!(
                "name: probe\nenvironment:\n  sdk: ^3.0.0\ndependencies:\n  demo:\n    hosted: {hosted_url}\n    version: ^1.0.0\n"
            ),
        )
        .await;
    let output = run(
        Tool::DartDocker,
        &[
            "sh",
            "-c",
            &format!("dart pub token add {hosted_url} --env-var PUB_TOKEN && dart pub get"),
        ],
        &[("PUB_TOKEN", alice.token.as_str())],
        project.path(),
    )
    .await;
    assert!(
        !output.success(),
        "the package does not exist: {}",
        output.combined()
    );
    let text = output.combined().to_lowercase();
    assert!(
        text.contains("not found") || text.contains("404") || text.contains("could not find"),
        "an authenticated request for a missing package is a 404: {text}"
    );
    assert!(
        !text.contains("authentication") && !text.contains("unauthorized") && !text.contains("401"),
        "the token must have been sent: {text}"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_login_uses_the_token_endpoint_and_never_touches_the_users_credentials() {
    require_tool!(Tool::Docker);
    let server = TestServer::builder()
        .bind_all(true)
        .register(Arc::new(RawLikeHandler::for_format(RepoFormat::Docker)))
        .start()
        .await;
    let alice = server.add_user("alice", &["nx-developer"]).await;
    let project = TempProject::new();
    // The daemon runs the login, so the address must be one the daemon can reach and trusts
    // as an insecure registry: loopback is, on a daemon that shares the host's network.
    let registry = format!("127.0.0.1:{}", server.port());
    let output = run(
        Tool::Docker,
        &["login", &registry, "-u", "alice", "-p", &alice.token],
        &[],
        project.path(),
    )
    .await;
    output.assert_success();
    assert!(
        output.combined().contains("Login Succeeded"),
        "{}",
        output.combined()
    );
    // The credential went to the isolated configuration, not to ~/.docker.
    let stored =
        String::from_utf8(project.read(".docker-config/config.json").await.unwrap()).unwrap();
    assert!(stored.contains(&registry), "{stored}");
    server.shutdown().await;
}
