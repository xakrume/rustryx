//! Hosted Helm repositories through the real format engine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "test scenarios read best as straight-line code"
)]

mod support;

use http::StatusCode;
use rustryx_core::model::WritePolicy;
use rustryx_format_helm::HelmHandler;
use rustryx_format_helm::chart::ChartLimits;
use rustryx_format_helm::model::IndexFile;
use rustryx_format_helm::testing::{archive, chart, chart_yaml, gzip, tar_entry};
use rustryx_format_helm::yaml::{YamlLimits, parse};
use sha2::{Digest, Sha256};
use support::{World, anonymous, body_bytes, header_of, principal};

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn error_text(response: http::Response<axum_core::body::Body>) -> String {
    let bytes = body_bytes(response).await;
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn index_of(
    world: &World,
    repo: &std::sync::Arc<rustryx_core::model::Repository>,
    who: &rustryx_core::identity::Principal,
) -> IndexFile {
    let response = world.get(repo, "index.yaml", who).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body_bytes(response).await;
    parse(&bytes, YamlLimits::index(1 << 20)).expect("index yaml")
}

#[tokio::test]
async fn an_uploaded_chart_is_stored_under_its_canonical_name_and_listed() {
    let world = World::new();
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    let bytes = chart("web", "1.2.3");
    let put = world
        .put(&repo, "charts/web-1.2.3.tgz", &admin, bytes.clone())
        .await;
    assert_eq!(put.status(), StatusCode::CREATED);

    let got = world.get(&repo, "charts/web-1.2.3.tgz", &admin).await;
    assert_eq!(got.status(), StatusCode::OK);
    assert_eq!(header_of(&got, "content-type"), Some("application/gzip"));
    assert_eq!(&body_bytes(got).await[..], bytes.as_slice());

    let head = world.head_of(&repo, "charts/web-1.2.3.tgz", &admin).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(
        header_of(&head, "content-length").map(str::parse::<usize>),
        Some(Ok(bytes.len()))
    );
    assert!(body_bytes(head).await.is_empty());

    let index = index_of(&world, &repo, &admin).await;
    assert_eq!(index.api_version, "v1");
    let entry = &index.entries["web"][0];
    assert_eq!(entry.version, "1.2.3");
    assert_eq!(entry.digest.as_deref(), Some(sha256_hex(&bytes).as_str()));
    assert_eq!(
        entry.urls,
        ["https://rustryx.test/repository/charts/charts/web-1.2.3.tgz"]
    );
    assert_eq!(entry.description.as_deref(), Some("Test chart web"));
    assert!(entry.created.is_some());
    assert!(index.generated.is_some());

    let head = world.head_of(&repo, "index.yaml", &admin).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(header_of(&head, "content-type"), Some("application/yaml"));
    let length = header_of(&head, "content-length")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    assert!(body_bytes(head).await.is_empty());
    let full = body_bytes(world.get(&repo, "index.yaml", &admin).await).await;
    assert_eq!(full.len(), length);
}

#[tokio::test]
async fn index_yaml_cannot_be_uploaded_or_deleted() {
    let world = World::new();
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    let marker = b"apiVersion: v1\nentries:\n  injected:\n  - name: injected\n    version: 9.9.9\n";
    let put = world
        .put(&repo, "index.yaml", &admin, marker.as_slice())
        .await;
    assert_eq!(put.status(), StatusCode::BAD_REQUEST);
    let message = error_text(put).await;
    assert!(message.contains("generated"), "{message}");

    let index = index_of(&world, &repo, &admin).await;
    assert!(index.entries.is_empty());
    let text = error_text(world.get(&repo, "index.yaml", &admin).await).await;
    assert!(!text.contains("injected"));

    let delete = world.delete(&repo, "index.yaml", &admin).await;
    assert_eq!(delete.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(header_of(&delete, "allow"), Some("GET, HEAD"));
}

#[tokio::test]
async fn a_chart_name_that_is_not_a_dns_label_is_refused() {
    let world = World::new();
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    for path in [
        "charts/Web-1.0.0.tgz",
        "charts/web-1.2.tgz",
        "charts/upload.tgz",
    ] {
        let response = world.put(&repo, path, &admin, chart("web", "1.0.0")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        let message = error_text(response).await;
        assert!(message.contains("DNS label"), "{path}: {message}");
    }
    let yaml = chart_yaml("Web", "1.0.0");
    let bytes = archive(&[("Web/Chart.yaml", yaml.as_bytes())]);
    let response = world
        .put(&repo, "charts/web-1.0.0.tgz", &admin, bytes)
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let message = error_text(response).await;
    assert!(message.contains("lowercase"), "{message}");
    assert!(index_of(&world, &repo, &admin).await.entries.is_empty());
}

#[tokio::test]
async fn the_archive_must_match_the_file_name() {
    let world = World::new();
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    let response = world
        .put(&repo, "charts/web-1.0.0.tgz", &admin, chart("web", "2.0.0"))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let message = error_text(response).await;
    assert!(message.contains("does not match"), "{message}");
    assert_eq!(
        world
            .get(&repo, "charts/web-2.0.0.tgz", &admin)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .get(&repo, "charts/web-1.0.0.tgz", &admin)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn path_traversal_is_400() {
    let world = World::new();
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    for path in [
        "charts/../index.yaml",
        "charts/%2e%2e/secret.tgz",
        "..%2findex.yaml",
    ] {
        let response = world.get(&repo, path, &admin).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        let message = error_text(response).await;
        assert!(message.contains("invalid path"), "{path}: {message}");
    }
}

#[tokio::test]
async fn a_tar_slip_and_a_gzip_bomb_are_refused_and_not_stored() {
    let limits = ChartLimits {
        max_decompressed_bytes: 16 * 1024,
        ..ChartLimits::default()
    };
    let world = World::with_handler(HelmHandler::new().with_chart_limits(limits));
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    let yaml = chart_yaml("web", "1.0.0");
    let slipped = archive(&[
        ("web/Chart.yaml", yaml.as_bytes()),
        ("web/../../etc/passwd", b"root"),
    ]);
    let slip = world
        .put(&repo, "charts/web-1.0.0.tgz", &admin, slipped)
        .await;
    assert_eq!(slip.status(), StatusCode::BAD_REQUEST);
    let message = error_text(slip).await;
    assert!(message.contains("unsafe path"), "{message}");

    let zeros = vec![0u8; 64 * 1024];
    let mut raw = tar_entry("web/Chart.yaml", yaml.as_bytes(), b'0', "");
    raw.extend_from_slice(&tar_entry("web/big.bin", &zeros, b'0', ""));
    raw.extend_from_slice(&[0u8; 1024]);
    let bomb = world
        .put(&repo, "charts/web-1.0.0.tgz", &admin, gzip(&raw))
        .await;
    assert_eq!(bomb.status(), StatusCode::BAD_REQUEST);
    let message = error_text(bomb).await;
    assert!(
        message.contains("compression ratio") || message.contains("allowed size"),
        "{message}"
    );
    assert!(index_of(&world, &repo, &admin).await.entries.is_empty());
}

#[tokio::test]
async fn versions_are_newest_first_and_delete_removes_the_chart_from_the_index() {
    let world = World::new();
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    for version in ["1.0.0", "1.2.0"] {
        let response = world
            .put(
                &repo,
                &format!("charts/web-{version}.tgz"),
                &admin,
                chart("web", version),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED, "{version}");
    }
    let index = index_of(&world, &repo, &admin).await;
    let versions: Vec<_> = index.entries["web"]
        .iter()
        .map(|entry| entry.version.as_str())
        .collect();
    assert_eq!(versions, ["1.2.0", "1.0.0"]);

    let deleted = world.delete(&repo, "charts/web-1.2.0.tgz", &admin).await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let index = index_of(&world, &repo, &admin).await;
    assert_eq!(index.entries["web"].len(), 1);
    assert_eq!(index.entries["web"][0].version, "1.0.0");
    assert_eq!(
        world
            .get(&repo, "charts/web-1.2.0.tgz", &admin)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_index_refuses_to_list_more_charts_than_its_cap() {
    let world = World::with_handler(HelmHandler::new().with_max_index_charts(1));
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    for (name, version) in [("web", "1.0.0"), ("db", "1.0.0")] {
        let response = world
            .put(
                &repo,
                &format!("charts/{name}-{version}.tgz"),
                &admin,
                chart(name, version),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED, "{name}");
    }
    let response = world.get(&repo, "index.yaml", &admin).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let message = error_text(response).await;
    assert!(message.contains("more charts"), "{message}");
    assert_eq!(
        world
            .get(&repo, "charts/db-1.0.0.tgz", &admin)
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn anonymous_is_401_a_reader_cannot_write_and_a_stranger_sees_nothing() {
    let world = World::new();
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    assert_eq!(
        world
            .put(&repo, "charts/web-1.0.0.tgz", &admin, chart("web", "1.0.0"))
            .await
            .status(),
        StatusCode::CREATED
    );

    assert_eq!(
        world.get(&repo, "index.yaml", &anonymous()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        world
            .get(&repo, "charts/web-1.0.0.tgz", &anonymous())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        world
            .put(
                &repo,
                "charts/web-1.0.1.tgz",
                &anonymous(),
                chart("web", "1.0.1")
            )
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );

    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    assert_eq!(
        world.get(&repo, "index.yaml", &reader).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        world
            .put(
                &repo,
                "charts/web-1.0.1.tgz",
                &reader,
                chart("web", "1.0.1")
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        world
            .delete(&repo, "charts/web-1.0.0.tgz", &reader)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    let stranger = principal("stranger");
    assert_eq!(
        world.get(&repo, "index.yaml", &stranger).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn allow_once_rejects_a_second_upload_of_the_same_chart() {
    let world = World::new();
    let repo = world.hosted_with("once", WritePolicy::AllowOnce).await;
    let admin = world.admin(&[&repo]);
    let first = chart("web", "1.0.0");
    assert_eq!(
        world
            .put(&repo, "charts/web-1.0.0.tgz", &admin, first.clone())
            .await
            .status(),
        StatusCode::CREATED
    );
    let yaml = chart_yaml("web", "1.0.0").replace("Test chart web", "replaced");
    let second = archive(&[("web/Chart.yaml", yaml.as_bytes())]);
    assert_eq!(
        world
            .put(&repo, "charts/web-1.0.0.tgz", &admin, second)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let got = body_bytes(world.get(&repo, "charts/web-1.0.0.tgz", &admin).await).await;
    assert_eq!(&got[..], first.as_slice());
}

#[tokio::test]
async fn an_upload_larger_than_the_archive_limit_is_413_and_is_not_stored() {
    let limits = ChartLimits {
        max_archive_bytes: 32,
        ..ChartLimits::default()
    };
    let world = World::with_handler(HelmHandler::new().with_chart_limits(limits));
    let repo = world.hosted("charts").await;
    let admin = world.admin(&[&repo]);
    let response = world
        .put(&repo, "charts/web-1.0.0.tgz", &admin, chart("web", "1.0.0"))
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(index_of(&world, &repo, &admin).await.entries.is_empty());
}
