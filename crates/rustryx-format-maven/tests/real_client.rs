//! Real `mvn` against the in-process harness server: deploy, resolve, snapshots, version ranges,
//! proxy over a wiremock upstream, groups, dependency confusion and the refusals a client sees.
//!
//! Gated by `RUSTRYX_IT=1`; without it every test prints `SKIP` and passes. The first run builds a
//! seeded local repository from Maven Central (see `mvn_support`), later runs are offline.
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

mod mvn_support;

use std::sync::Arc;

use rustryx_core::model::{RepoFormat, WritePolicy};
use rustryx_format_maven::MavenHandler;
use rustryx_testserver::{
    NewTestRepo, READ, TestPrivilege, TestServer, TestUser, Tool, UpstreamStub,
    loopback_friendly_policy,
};

use mvn_support::{MvnEnv, env, read_tree, write_consumer, write_project};

const GROUP: &str = "io.rustryx.it";
const GROUP_PATH: &str = "io/rustryx/it";

async fn start() -> TestServer {
    TestServer::builder()
        .register(Arc::new(MavenHandler::new()))
        .upstream_policy(loopback_friendly_policy())
        .start()
        .await
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().build().unwrap()
}

async fn get(url: &str, user: &TestUser) -> reqwest::Response {
    client()
        .get(url)
        .basic_auth(&user.username, Some(&user.token))
        .send()
        .await
        .unwrap()
}

fn releases() -> NewTestRepo {
    NewTestRepo::hosted("releases", RepoFormat::Maven2)
        .write_policy(WritePolicy::AllowOnce)
        .format_config(serde_json::json!({"version_policy": "release"}))
}

async fn env_for(user: &TestUser) -> Option<MvnEnv> {
    env(&[("rustryx", &user.username, &user.token)]).await
}

/// Deploys the project in the environment's root.
async fn deploy(env: &MvnEnv) -> rustryx_testserver::Output {
    env.mvn(None, &["deploy", "-DskipTests"]).await
}

fn resolve_args<'a>(artifact: &'a str, repository: &'a str) -> Vec<&'a str> {
    vec![
        "-C",
        "dependency:get",
        artifact,
        repository,
        "-Dtransitive=false",
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn mvn_deploys_a_release_with_sources_and_resolves_it_with_strict_checksums() {
    rustryx_testserver::require_tool!(Tool::Mvn);
    let server = start().await;
    server.add_repository(releases()).await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let url = server.repository_url("releases");
    let Some(publisher) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    write_project(
        &publisher.project,
        GROUP,
        "demo",
        "1.0",
        "Alpha",
        Some(&url),
    )
    .await;
    deploy(&publisher).await.assert_success();

    // The server holds the jar, the pom and the sources classifier, byte for byte.
    let built = tokio::fs::read(publisher.project.join("target/demo-1.0.jar"))
        .await
        .unwrap();
    for (file, expected) in [
        ("demo-1.0.jar", Some(built.clone())),
        ("demo-1.0.pom", None),
        ("demo-1.0-sources.jar", None),
    ] {
        let response = get(&format!("{url}/{GROUP_PATH}/demo/1.0/{file}"), &dev).await;
        assert_eq!(response.status(), 200, "{file}");
        if let Some(expected) = expected {
            assert_eq!(response.bytes().await.unwrap().to_vec(), expected);
        }
    }
    let metadata = get(&format!("{url}/{GROUP_PATH}/demo/maven-metadata.xml"), &dev)
        .await
        .text()
        .await
        .unwrap();
    assert!(metadata.contains("<version>1.0</version>"), "{metadata}");
    assert!(metadata.contains("<release>1.0</release>"));

    // A second release, so that a range has something to choose from.
    write_project(
        &publisher.project,
        GROUP,
        "demo",
        "1.1",
        "Alpha",
        Some(&url),
    )
    .await;
    deploy(&publisher).await.assert_success();

    // A different machine resolves with strict checksums: the derived checksum files are right.
    let Some(consumer) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    let repository = format!("-DremoteRepositories=rustryx::default::{url}");
    let one = format!("-Dartifact={GROUP}:demo:1.0");
    consumer
        .mvn(None, &resolve_args(&one, &repository))
        .await
        .assert_success();
    let resolved = tokio::fs::read(consumer.local(&format!("{GROUP_PATH}/demo/1.0/demo-1.0.jar")))
        .await
        .unwrap();
    assert_eq!(resolved, built);

    let sources = format!("-Dartifact={GROUP}:demo:1.0:jar:sources");
    consumer
        .mvn(None, &resolve_args(&sources, &repository))
        .await
        .assert_success();
    assert!(
        consumer
            .local(&format!("{GROUP_PATH}/demo/1.0/demo-1.0-sources.jar"))
            .exists()
    );

    // A version range is answered from the generated artifact metadata. (`dependency:get` does
    // not resolve ranges; a project that depends on one does.)
    let dir = write_consumer(&consumer.project, "range", GROUP, "demo", "[1.0,2.0)", &url).await;
    consumer
        .mvn(Some(&dir), &["-C", "dependency:resolve"])
        .await
        .assert_success();
    assert!(
        consumer
            .local(&format!("{GROUP_PATH}/demo/1.1/demo-1.1.jar"))
            .exists()
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mvn_dependency_resolve_walks_a_pom_that_depends_on_a_hosted_artifact() {
    rustryx_testserver::require_tool!(Tool::Mvn);
    let server = start().await;
    server.add_repository(releases()).await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let url = server.repository_url("releases");
    let Some(publisher) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    write_project(&publisher.project, GROUP, "lib", "3.0", "Beta", Some(&url)).await;
    deploy(&publisher).await.assert_success();

    let Some(consumer) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    let dir = write_consumer(&consumer.project, "consumer", GROUP, "lib", "3.0", &url).await;
    consumer
        .mvn(Some(&dir), &["-C", "dependency:resolve"])
        .await
        .assert_success();
    assert!(
        consumer
            .local(&format!("{GROUP_PATH}/lib/3.0/lib-3.0.jar"))
            .exists()
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn must_not_overwrite_a_release_a_second_deploy_is_a_409() {
    rustryx_testserver::require_tool!(Tool::Mvn);
    let server = start().await;
    server.add_repository(releases()).await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let url = server.repository_url("releases");
    let Some(publisher) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    write_project(
        &publisher.project,
        GROUP,
        "once",
        "1.0",
        "Alpha",
        Some(&url),
    )
    .await;
    deploy(&publisher).await.assert_success();
    let jar_url = format!("{url}/{GROUP_PATH}/once/1.0/once-1.0.jar");
    let before = get(&jar_url, &dev).await.bytes().await.unwrap();

    // Different content, same coordinate: the repository refuses it.
    write_project(
        &publisher.project,
        GROUP,
        "once",
        "1.0",
        "Gamma",
        Some(&url),
    )
    .await;
    let again = deploy(&publisher).await;
    assert!(!again.success(), "{}", again.combined());
    assert!(again.combined().contains("409"), "{}", again.combined());
    assert_eq!(get(&jar_url, &dev).await.bytes().await.unwrap(), before);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mvn_deploys_snapshots_and_resolves_the_newest_build() {
    rustryx_testserver::require_tool!(Tool::Mvn);
    let server = start().await;
    server
        .add_repository(
            NewTestRepo::hosted("snapshots", RepoFormat::Maven2)
                .format_config(serde_json::json!({"version_policy": "snapshot"})),
        )
        .await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let url = server.repository_url("snapshots");
    let Some(publisher) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    write_project(
        &publisher.project,
        GROUP,
        "snap",
        "1.0-SNAPSHOT",
        "Alpha",
        Some(&url),
    )
    .await;
    deploy(&publisher).await.assert_success();
    tokio::fs::remove_file(
        publisher
            .project
            .join("src/main/java/io/rustryx/it/Alpha.java"),
    )
    .await
    .unwrap();
    write_project(
        &publisher.project,
        GROUP,
        "snap",
        "1.0-SNAPSHOT",
        "Beta",
        Some(&url),
    )
    .await;
    deploy(&publisher).await.assert_success();

    let metadata = get(
        &format!("{url}/{GROUP_PATH}/snap/1.0-SNAPSHOT/maven-metadata.xml"),
        &dev,
    )
    .await
    .text()
    .await
    .unwrap();
    assert!(
        metadata.contains("<buildNumber>2</buildNumber>"),
        "{metadata}"
    );
    assert!(metadata.contains("<snapshotVersions>"));

    let built = tokio::fs::read(publisher.project.join("target/snap-1.0-SNAPSHOT.jar"))
        .await
        .unwrap();
    let Some(consumer) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    let repository = format!("-DremoteRepositories=rustryx::default::{url}");
    let artifact = format!("-Dartifact={GROUP}:snap:1.0-SNAPSHOT");
    consumer
        .mvn(None, &resolve_args(&artifact, &repository))
        .await
        .assert_success();
    let files = read_tree(&consumer.local(&format!("{GROUP_PATH}/snap/1.0-SNAPSHOT"))).await;
    let jar = files
        .iter()
        .find(|(name, _)| name.ends_with("-2.jar") && !name.contains("sources"))
        .unwrap_or_else(|| {
            panic!(
                "the newest build is in the local repository: {:?}",
                files.iter().map(|f| &f.0).collect::<Vec<_>>()
            )
        });
    assert_eq!(jar.1, built);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mvn_resolves_through_a_proxy_and_a_group_and_the_group_resists_dependency_confusion() {
    rustryx_testserver::require_tool!(Tool::Mvn);
    let server = start().await;
    server.add_repository(releases()).await;
    let upstream = UpstreamStub::start().await;
    server
        .add_repository(NewTestRepo::proxy(
            "mirror",
            RepoFormat::Maven2,
            &upstream.base_url(),
        ))
        .await;
    server
        .add_repository(NewTestRepo::group(
            "all",
            RepoFormat::Maven2,
            &["releases", "mirror"],
        ))
        .await;
    let dev = server.add_user("dev", &["nx-developer"]).await;
    let url = server.repository_url("releases");

    // The upstream repository: built by Maven itself into a file repository, then served
    // verbatim (layout, metadata and checksum files as Maven writes them).
    let Some(builder) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    let remote_dir = builder.project.join("remote-repo");
    for (artifact, version) in [("remote", "2.0"), ("demo", "99.0")] {
        write_project(&builder.project, GROUP, artifact, version, "Remote", None).await;
        let alt = format!(
            "-DaltDeploymentRepository=local::file://{}",
            remote_dir.display()
        );
        builder
            .mvn(None, &["deploy", "-DskipTests", &alt])
            .await
            .assert_success();
    }
    for (relative, bytes) in read_tree(&remote_dir).await {
        let ctype = if relative.ends_with(".xml") {
            "application/xml"
        } else {
            "application/octet-stream"
        };
        upstream
            .serve_file(&format!("/{relative}"), bytes, ctype)
            .await;
    }

    // The internal artifact, with the same name the attacker publishes upstream.
    write_project(&builder.project, GROUP, "demo", "1.0", "Alpha", Some(&url)).await;
    deploy(&builder).await.assert_success();

    // 1. Through the proxy, twice from different machines: the upstream is asked once for the file.
    let proxy_url = server.repository_url("mirror");
    let repository = format!("-DremoteRepositories=rustryx::default::{proxy_url}");
    let artifact = format!("-Dartifact={GROUP}:remote:2.0");
    for _ in 0..2 {
        let Some(consumer) = env_for(&dev).await else {
            server.shutdown().await;
            return;
        };
        consumer
            .mvn(None, &resolve_args(&artifact, &repository))
            .await
            .assert_success();
        assert!(
            consumer
                .local(&format!("{GROUP_PATH}/remote/2.0/remote-2.0.jar"))
                .exists()
        );
    }
    server.settle().await;
    assert_eq!(
        upstream
            .count("GET", &format!("/{GROUP_PATH}/remote/2.0/remote-2.0.jar"))
            .await,
        1,
        "the second machine is served from the cache"
    );

    // 2. Through the group: a name only the proxy has comes from the proxy.
    let group_url = server.repository_url("all");
    let repository = format!("-DremoteRepositories=rustryx::default::{group_url}");
    let Some(consumer) = env_for(&dev).await else {
        server.shutdown().await;
        return;
    };
    consumer
        .mvn(None, &resolve_args(&artifact, &repository))
        .await
        .assert_success();

    // 3. The internal name: the range sees only the internal versions and never asks the
    // upstream about the name, and the attacker's 99.0 does not exist for the group.
    let before = upstream.request_count().await;
    let dir = write_consumer(
        &consumer.project,
        "range",
        GROUP,
        "demo",
        "[1.0,)",
        &group_url,
    )
    .await;
    consumer
        .mvn(Some(&dir), &["-C", "dependency:resolve"])
        .await
        .assert_success();
    assert!(
        consumer
            .local(&format!("{GROUP_PATH}/demo/1.0/demo-1.0.jar"))
            .exists()
    );
    assert!(!consumer.local(&format!("{GROUP_PATH}/demo/99.0")).exists());
    let evil = format!("-Dartifact={GROUP}:demo:99.0");
    let refused = consumer.mvn(None, &resolve_args(&evil, &repository)).await;
    assert!(!refused.success(), "{}", refused.combined());
    assert_eq!(
        upstream.request_count().await,
        before,
        "the public repository was not asked about the claimed name"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mvn_deploy_without_credentials_is_a_401_and_without_the_privilege_a_403() {
    rustryx_testserver::require_tool!(Tool::Mvn);
    let server = start().await;
    server.add_repository(releases()).await;
    let role = server
        .add_role_with_privileges("readers", &[TestPrivilege::repository_view("*", &READ)])
        .await;
    let reader = server.add_user("reader", &[role.name.as_str()]).await;
    let url = server.repository_url("releases");

    // No credentials at all: the client is challenged and, having none to offer, fails with 401.
    let Some(anonymous) = env(&[]).await else {
        server.shutdown().await;
        return;
    };
    write_project(
        &anonymous.project,
        GROUP,
        "denied",
        "1.0",
        "Alpha",
        Some(&url),
    )
    .await;
    let output = deploy(&anonymous).await;
    assert!(!output.success());
    assert!(output.combined().contains("401"), "{}", output.combined());

    // Valid credentials without the write privilege: 403, and the credentials are not the problem.
    let Some(as_reader) = env_for(&reader).await else {
        server.shutdown().await;
        return;
    };
    write_project(
        &as_reader.project,
        GROUP,
        "denied",
        "1.0",
        "Alpha",
        Some(&url),
    )
    .await;
    let output = deploy(&as_reader).await;
    assert!(!output.success());
    assert!(output.combined().contains("403"), "{}", output.combined());
    server.shutdown().await;
}
