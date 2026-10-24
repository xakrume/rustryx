//! Contract tests of the proxy repository, through `FormatEngine` over a scripted upstream.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::large_futures,
    clippy::duration_suboptimal_units,
    reason = "test scenarios read best as straight-line code"
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum_core::body::Body;
use http::{Method, StatusCode, header};
use rustryx_core::model::Repository;
use rustryx_format_core::testing::Reply;
use rustryx_format_pub::testing::{package_archive, sha256_hex};
use support::{
    BASE, Offered, PUB_JSON, World, anonymous, answer, path_below, principal, script_upstream,
    tracked_body, upstream_listing,
};

async fn proxied() -> (World, Arc<Repository>, rustryx_core::identity::Principal) {
    let world = World::new();
    let repo = world.proxy_at("pub-proxy", "https://pub.example").await;
    world.rbac.grant_read("carol", "pub-proxy");
    (world, repo, principal("carol"))
}

#[tokio::test]
async fn the_listing_is_rewritten_to_this_repository_and_the_archive_is_served() {
    let (world, repo, carol) = proxied().await;
    let one = package_archive("demo", "1.0.0");
    let two = package_archive("demo", "1.1.0");
    script_upstream(
        &world,
        "demo",
        &[Offered::new("1.0.0", &one), Offered::new("1.1.0", &two)],
    );
    let mut head = world.head(Method::GET, "api/packages/demo", &carol);
    // What a client sends must stay with the client.
    head.headers.insert(
        header::AUTHORIZATION,
        "Bearer ryx_client_secret".parse().unwrap(),
    );
    head.headers
        .insert(header::COOKIE, "session=abc".parse().unwrap());
    let list = answer(world.serve(&repo, head, Body::empty()).await).await;
    assert_eq!(
        list.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&list.body)
    );
    assert_eq!(list.header("content-type"), Some(PUB_JSON));
    let json = list.json();
    assert_eq!(json["name"], "demo");
    assert_eq!(json["latest"]["version"], "1.1.0");
    let prefix = format!("{BASE}/repository/pub-proxy/");
    for entry in json["versions"].as_array().unwrap() {
        let url = entry["archive_url"].as_str().unwrap();
        assert!(url.starts_with(&prefix), "{url}");
        assert!(
            !url.contains("pub.example"),
            "the upstream address leaked: {url}"
        );
    }
    assert_eq!(json["versions"][0]["archive_sha256"], sha256_hex(&one));
    assert!(!json.to_string().contains("advisoriesUpdated"));

    // The upstream was asked the way the protocol says, with nothing of the client's.
    let asked = world.upstream.requests();
    let request = asked
        .iter()
        .find(|r| r.path == "/api/packages/demo")
        .unwrap();
    assert_eq!(request.headers[header::ACCEPT], PUB_JSON);
    assert!(!request.headers.contains_key(header::AUTHORIZATION));
    assert!(!request.headers.contains_key(header::COOKIE));

    // Download from the rewritten URL, then again: the second one is a cache hit.
    let path = path_below(&repo, json["versions"][0]["archive_url"].as_str().unwrap());
    let first = world.get(&repo, &path, &carol).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.body.as_ref(), one.as_slice());
    let second = world.get(&repo, &path, &carol).await;
    assert_eq!(second.body.as_ref(), one.as_slice());
    assert_eq!(second.header("x-rustryx-cache"), Some("HIT"));
    world.settle().await;
    assert_eq!(
        world
            .upstream
            .calls_for("/packages/demo/versions/1.0.0.tar.gz"),
        1
    );
    assert_eq!(
        world.upstream.calls_for("/api/packages/demo"),
        1,
        "the listing is cached too"
    );

    // The single version document is derived from the same cached listing.
    let single = world
        .get(&repo, "api/packages/demo/versions/1.1.0", &carol)
        .await;
    assert_eq!(single.status, StatusCode::OK);
    assert_eq!(single.json()["version"], "1.1.0");
    assert!(
        single.json()["archive_url"]
            .as_str()
            .unwrap()
            .starts_with(&prefix)
    );
    assert_eq!(world.upstream.calls_for("/api/packages/demo"), 1);
    world
        .get(&repo, "api/packages/demo/versions/9.9.9", &carol)
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn must_not_serve_an_archive_whose_checksum_differs_from_the_listing() {
    let (world, repo, carol) = proxied().await;
    let honest = package_archive("demo", "1.0.0");
    let tampered = package_archive("demo", "1.0.0-tampered");
    script_upstream(&world, "demo", &[Offered::new("1.0.0", &honest)]);
    // The listing says one thing, the file is another.
    world.upstream.route(
        "/packages/demo/versions/1.0.0.tar.gz",
        vec![Reply::ok(tampered.clone())],
    );
    let refused = world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &carol)
        .await;
    refused.assert_pub_error(StatusCode::BAD_GATEWAY);
    assert!(!String::from_utf8_lossy(&refused.body).contains("tampered"));
    world.settle().await;
    assert_eq!(
        world.asset_paths(&repo, "packages/").await,
        Vec::<String>::new(),
        "a file that failed its checksum must not be cached"
    );
    // And it is not served from anywhere else on the next request either.
    let again = world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &carol)
        .await;
    assert_eq!(again.status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn must_not_fetch_an_archive_the_upstream_does_not_list() {
    let (world, repo, carol) = proxied().await;
    let one = package_archive("demo", "1.0.0");
    script_upstream(&world, "demo", &[Offered::new("1.0.0", &one)]);
    world.upstream.route(
        "/packages/demo/versions/9.9.9.tar.gz",
        vec![Reply::ok(b"smuggled".to_vec())],
    );
    world
        .get(&repo, "packages/demo/versions/9.9.9.tar.gz", &carol)
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
    assert_eq!(
        world
            .upstream
            .calls_for("/packages/demo/versions/9.9.9.tar.gz"),
        0
    );
}

#[tokio::test]
async fn an_archive_without_a_digest_in_the_listing_is_served_and_listed_without_one() {
    let (world, repo, carol) = proxied().await;
    let one = package_archive("demo", "1.0.0");
    let mut listing: serde_json::Value =
        serde_json::from_slice(&upstream_listing("demo", &[Offered::new("1.0.0", &one)])).unwrap();
    listing["versions"][0]
        .as_object_mut()
        .unwrap()
        .remove("archive_sha256");
    world.upstream.route(
        "/api/packages/demo",
        vec![Reply::ok(serde_json::to_vec(&listing).unwrap())],
    );
    world.upstream.route(
        "/packages/demo/versions/1.0.0.tar.gz",
        vec![Reply::ok(one.clone())],
    );
    let list = world.list(&repo, "demo", &carol).await.json();
    assert!(list["versions"][0].get("archive_sha256").is_none());
    let download = world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &carol)
        .await;
    assert_eq!(download.body.as_ref(), one.as_slice());
}

#[tokio::test]
async fn a_missing_package_is_remembered_and_not_asked_for_again() {
    let (world, repo, carol) = proxied().await;
    for _ in 0..3 {
        world
            .list(&repo, "nothing", &carol)
            .await
            .assert_pub_error(StatusCode::NOT_FOUND);
    }
    world.settle().await;
    assert_eq!(world.upstream.calls_for("/api/packages/nothing"), 1);
}

#[tokio::test]
async fn must_not_forward_an_upstream_401_or_403_to_the_client() {
    for code in [401u16, 403] {
        let (world, repo, carol) = proxied().await;
        world
            .upstream
            .route("/api/packages/demo", vec![Reply::status(code)]);
        let answer = world.list(&repo, "demo", &carol).await;
        // The client's token is fine; a 401 or 403 here would make it throw the token away or
        // retry with another. The failure is ours to report, as a bad gateway.
        answer.assert_pub_error(StatusCode::BAD_GATEWAY);
        assert!(answer.header("www-authenticate").is_none(), "{code}");
        assert_ne!(answer.error_code(), "unauthenticated");
        assert!(!answer.error_message().contains("pub.example"));
    }
    // The same for an archive.
    let (world, repo, carol) = proxied().await;
    let one = package_archive("demo", "1.0.0");
    script_upstream(&world, "demo", &[Offered::new("1.0.0", &one)]);
    world.upstream.route(
        "/packages/demo/versions/1.0.0.tar.gz",
        vec![Reply::status(403)],
    );
    let answer = world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &carol)
        .await;
    answer.assert_pub_error(StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn an_unusable_listing_is_a_bad_gateway_that_says_nothing_about_the_upstream() {
    let (world, repo, carol) = proxied().await;
    for (label, body) in [
        (
            "html",
            b"<html>502 Bad Gateway nginx/1.2 at 10.9.9.9</html>".to_vec(),
        ),
        ("array", b"[1,2,3]".to_vec()),
        (
            "another package",
            upstream_listing("other", &[Offered::new("1.0.0", b"x")]),
        ),
        ("no versions", br#"{"name":"demo"}"#.to_vec()),
    ] {
        world
            .upstream
            .route("/api/packages/demo", vec![Reply::ok(body)]);
        // A fresh repository per case: the first answer would be cached.
        let repo = world
            .proxy_at(
                &format!("proxy-{}", label.replace(' ', "-")),
                "https://pub.example",
            )
            .await;
        world.rbac.grant_read("carol", repo.name.as_str());
        let answer = world.list(&repo, "demo", &carol).await;
        answer.assert_pub_error(StatusCode::BAD_GATEWAY);
        let text = String::from_utf8_lossy(&answer.body).into_owned();
        assert!(
            !text.contains("10.9.9.9") && !text.contains("nginx"),
            "{label}: {text}"
        );
    }
    drop(repo);
}

#[tokio::test]
async fn entries_that_cannot_be_trusted_are_left_out_of_the_listing() {
    let (world, repo, carol) = proxied().await;
    let one = package_archive("demo", "1.0.0");
    let mut listing: serde_json::Value =
        serde_json::from_slice(&upstream_listing("demo", &[Offered::new("1.0.0", &one)])).unwrap();
    let good = listing["versions"][0].clone();
    let mut bad_version = good.clone();
    bad_version["version"] = "../../etc/passwd".into();
    let mut other_manifest = good.clone();
    other_manifest["version"] = "2.0.0".into();
    other_manifest["pubspec"]["name"] = "other".into();
    let mut bad_digest = good.clone();
    bad_digest["version"] = "3.0.0".into();
    bad_digest["pubspec"]["version"] = "3.0.0".into();
    bad_digest["archive_sha256"] = "zz".into();
    listing["versions"] = serde_json::json!([good, bad_version, other_manifest, bad_digest]);
    world.upstream.route(
        "/api/packages/demo",
        vec![Reply::ok(serde_json::to_vec(&listing).unwrap())],
    );
    let list = world.list(&repo, "demo", &carol).await.json();
    let versions: Vec<&str> = list["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["1.0.0"]);
}

#[tokio::test]
async fn retraction_and_discontinuation_pass_through_from_the_upstream() {
    let (world, repo, carol) = proxied().await;
    let one = package_archive("demo", "1.0.0");
    let two = package_archive("demo", "2.0.0");
    let mut listing: serde_json::Value = serde_json::from_slice(&upstream_listing(
        "demo",
        &[
            Offered::new("1.0.0", &one),
            Offered::new("2.0.0", &two).retracted(),
        ],
    ))
    .unwrap();
    listing["isDiscontinued"] = true.into();
    listing["replacedBy"] = "successor".into();
    world.upstream.route(
        "/api/packages/demo",
        vec![Reply::ok(serde_json::to_vec(&listing).unwrap())],
    );
    let list = world.list(&repo, "demo", &carol).await.json();
    assert_eq!(list["isDiscontinued"], true);
    assert_eq!(list["replacedBy"], "successor");
    assert_eq!(list["versions"][0]["retracted"], false);
    assert_eq!(list["versions"][1]["retracted"], true);
    assert_eq!(
        list["latest"]["version"], "1.0.0",
        "a retracted version is not the latest"
    );
}

#[tokio::test]
async fn an_expired_listing_is_served_stale_when_the_upstream_is_down() {
    let (world, repo, carol) = proxied().await;
    let one = package_archive("demo", "1.0.0");
    script_upstream(&world, "demo", &[Offered::new("1.0.0", &one)]);
    assert_eq!(
        world.list(&repo, "demo", &carol).await.status,
        StatusCode::OK
    );
    world.settle().await;
    // Past the metadata freshness of the fixture repository (600 s), with the upstream failing.
    world.clock.advance(Duration::from_secs(900));
    world
        .upstream
        .route("/api/packages/demo", vec![Reply::status(503)]);
    let stale = world.list(&repo, "demo", &carol).await;
    assert_eq!(stale.status, StatusCode::OK);
    assert_eq!(stale.header("x-rustryx-cache"), Some("STALE"));
    assert!(stale.header("warning").unwrap().starts_with("110"));
    assert_eq!(stale.json()["versions"][0]["version"], "1.0.0");
}

#[tokio::test]
async fn an_unreachable_upstream_without_a_cached_copy_is_a_bad_gateway() {
    let (world, repo, carol) = proxied().await;
    world.upstream.route(
        "/api/packages/demo",
        vec![Reply::error(
            rustryx_format_core::proxy::upstream::UpstreamError::Connect("refused".into()),
        )],
    );
    world
        .list(&repo, "demo", &carol)
        .await
        .assert_pub_error(StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn must_not_accept_a_publication_on_a_proxy_and_must_not_read_its_body() {
    let (world, repo, carol) = proxied().await;
    world.rbac.grant_all("carol", "pub-proxy");
    for (method, path) in [
        (Method::GET, "api/packages/versions/new"),
        (
            Method::POST,
            "api/packages/versions/upload/0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10",
        ),
        (
            Method::GET,
            "api/packages/versions/finish/0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10",
        ),
    ] {
        let (polled, body) = tracked_body(b"archive bytes");
        let answer = world.send(&repo, method.clone(), path, &carol, body).await;
        answer.assert_pub_error(StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(answer.header("allow"), Some("GET, HEAD"));
        assert!(!polled.load(Ordering::SeqCst), "{method} {path}");
    }
    assert_eq!(world.upstream.calls(), 0);
}

#[tokio::test]
async fn a_caller_without_read_rights_gets_403_from_a_proxy_too() {
    let (world, repo, _) = proxied().await;
    let bob = principal("bob");
    world
        .list(&repo, "demo", &bob)
        .await
        .assert_pub_error(StatusCode::FORBIDDEN);
    world
        .get(&repo, "packages/demo/versions/1.0.0.tar.gz", &anonymous())
        .await
        .assert_pub_error(StatusCode::UNAUTHORIZED);
    assert_eq!(
        world.upstream.calls(),
        0,
        "nothing is fetched for a caller who may not read"
    );
}

#[tokio::test]
async fn head_on_a_listing_has_headers_and_no_body() {
    let (world, repo, carol) = proxied().await;
    let one = package_archive("demo", "1.0.0");
    script_upstream(&world, "demo", &[Offered::new("1.0.0", &one)]);
    let get = world.list(&repo, "demo", &carol).await;
    let head = world
        .send(
            &repo,
            Method::HEAD,
            "api/packages/demo",
            &carol,
            Body::empty(),
        )
        .await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(
        head.header("content-length"),
        Some(get.body.len().to_string().as_str())
    );
    assert_eq!(head.header("etag"), get.header("etag"));
}
