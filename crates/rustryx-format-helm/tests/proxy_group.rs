//! Proxy and group Helm repositories through the real format engine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "test scenarios read best as straight-line code"
)]

mod support;

use http::{Method, StatusCode};
use rustryx_format_core::testing::Reply;
use rustryx_format_helm::model::IndexFile;
use rustryx_format_helm::testing::chart;
use rustryx_format_helm::yaml::{YamlLimits, parse};
use support::{World, anonymous, body_bytes, header_of, principal};

fn upstream_index() -> String {
    "\
apiVersion: v1
entries:
  web:
  - name: web
    version: 9.9.9
    description: public web
    urls:
    - https://upstream.example.test/charts/web-9.9.9.tgz
  other:
  - name: other
    version: 1.0.0
    description: public other
    urls:
    - https://upstream.example.test/charts/other-1.0.0.tgz
generated: \"2020-01-01T00:00:00Z\"
"
    .to_owned()
}

#[tokio::test]
async fn a_proxy_rewrites_index_urls_and_caches_the_chart() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    world
        .upstream
        .route("/index.yaml", vec![Reply::ok(upstream_index())]);
    let archive = b"chart-bytes";
    world
        .upstream
        .route("/charts/web-9.9.9.tgz", vec![Reply::ok(archive.as_slice())]);

    let first = world.get(&repo, "index.yaml", &reader).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(header_of(&first, "x-rustryx-cache"), Some("MISS"));
    assert_eq!(header_of(&first, "content-type"), Some("application/yaml"));
    let index: IndexFile = parse(&body_bytes(first).await, YamlLimits::index(1 << 20)).unwrap();
    let web = &index.entries["web"][0];
    assert_eq!(web.version, "9.9.9");
    assert_eq!(
        web.urls,
        ["https://rustryx.test/repository/mirror/charts/web-9.9.9.tgz"]
    );
    assert!(!web.urls[0].contains("upstream.example.test"));
    assert_eq!(
        index.entries["other"][0].urls,
        ["https://rustryx.test/repository/mirror/charts/other-1.0.0.tgz"]
    );

    world.settle().await;
    let second = world.get(&repo, "index.yaml", &reader).await;
    assert_eq!(header_of(&second, "x-rustryx-cache"), Some("HIT"));
    assert_eq!(world.upstream.calls_for("/index.yaml"), 1);

    let downloaded = world.get(&repo, "charts/web-9.9.9.tgz", &reader).await;
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(&body_bytes(downloaded).await[..], archive);
    world.settle().await;
    let again = world.get(&repo, "charts/web-9.9.9.tgz", &reader).await;
    assert_eq!(header_of(&again, "x-rustryx-cache"), Some("HIT"));
    assert_eq!(world.upstream.calls_for("/charts/web-9.9.9.tgz"), 1);

    let put = world
        .put(
            &repo,
            "charts/web-1.0.0.tgz",
            &reader,
            chart("web", "1.0.0"),
        )
        .await;
    assert_eq!(put.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(header_of(&put, "allow"), Some("GET, HEAD"));
}

#[tokio::test]
async fn a_group_prefers_a_hosted_chart_name_over_a_later_proxy() {
    let world = World::new();
    let hosted = world.hosted("internal").await;
    let proxy = world.proxy("public").await;
    let group = world.group("all", &[&hosted, &proxy]).await;
    let admin = world.admin(&[&hosted]);
    assert_eq!(
        world
            .put(
                &hosted,
                "charts/web-1.0.0.tgz",
                &admin,
                chart("web", "1.0.0")
            )
            .await
            .status(),
        StatusCode::CREATED
    );
    world
        .upstream
        .route("/index.yaml", vec![Reply::ok(upstream_index())]);
    world
        .upstream
        .route("/charts/web-9.9.9.tgz", vec![Reply::ok("attack")]);
    world
        .upstream
        .route("/charts/other-1.0.0.tgz", vec![Reply::ok("other-bytes")]);
    world.grant_read("reader", &[&group, &hosted, &proxy]);
    let reader = principal("reader");

    let response = world.get(&group, "index.yaml", &reader).await;
    assert_eq!(response.status(), StatusCode::OK);
    let index: IndexFile = parse(&body_bytes(response).await, YamlLimits::index(1 << 20)).unwrap();
    let web: Vec<_> = index.entries["web"]
        .iter()
        .map(|entry| entry.version.as_str())
        .collect();
    assert_eq!(web, ["1.0.0"], "the public 9.9.9 must not be merged in");
    assert_eq!(
        index.entries["web"][0].urls,
        ["https://rustryx.test/repository/all/charts/web-1.0.0.tgz"]
    );
    assert_eq!(index.entries["other"][0].version, "1.0.0");
    assert!(
        index.entries["other"][0].urls[0].contains("/repository/all/charts/other-1.0.0.tgz"),
        "{:?}",
        index.entries["other"][0].urls
    );

    let hosted_chart = world.get(&group, "charts/web-1.0.0.tgz", &reader).await;
    assert_eq!(hosted_chart.status(), StatusCode::OK);
    assert_eq!(
        &body_bytes(hosted_chart).await[..],
        chart("web", "1.0.0").as_slice()
    );
    let attack = world.get(&group, "charts/web-9.9.9.tgz", &reader).await;
    assert_eq!(attack.status(), StatusCode::NOT_FOUND);
    assert_eq!(world.upstream.calls_for("/charts/web-9.9.9.tgz"), 0);
    let other = world.get(&group, "charts/other-1.0.0.tgz", &reader).await;
    assert_eq!(other.status(), StatusCode::OK);
    assert_eq!(&body_bytes(other).await[..], b"other-bytes");

    assert_eq!(
        world
            .send(
                &group,
                Method::PUT,
                "index.yaml",
                &reader,
                axum_core::body::Body::from("x")
            )
            .await
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        world.get(&group, "index.yaml", &anonymous()).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn deleting_a_hosted_chart_keeps_the_name_claimed() {
    let world = World::new();
    let hosted = world.hosted("internal").await;
    let proxy = world.proxy("public").await;
    let group = world.group("all", &[&hosted, &proxy]).await;
    let admin = world.admin(&[&hosted, &group, &proxy]);
    world
        .put(
            &hosted,
            "charts/web-1.0.0.tgz",
            &admin,
            chart("web", "1.0.0"),
        )
        .await;
    assert_eq!(
        world
            .delete(&hosted, "charts/web-1.0.0.tgz", &admin)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    world
        .upstream
        .route("/charts/web-9.9.9.tgz", vec![Reply::ok("attack")]);
    world.grant_read("reader", &[&group, &hosted, &proxy]);
    let reader = principal("reader");
    let attack = world.get(&group, "charts/web-9.9.9.tgz", &reader).await;
    assert_eq!(attack.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        world.upstream.calls_for("/charts/web-9.9.9.tgz"),
        0,
        "the component row still claims the name"
    );
}
