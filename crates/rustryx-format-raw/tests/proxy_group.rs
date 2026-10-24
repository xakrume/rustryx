//! Proxy and group repositories of the raw format, through the real engine.
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

mod support;

use std::time::Duration;

use axum_core::body::Body;
use http::{Method, StatusCode};
use rustryx_format_core::testing::Reply;
use support::{
    World, anonymous, body_bytes, body_json, body_text, entry_names, header_of, principal,
};

const JSON: [(&str, &str); 1] = [("accept", "application/json")];

fn sha256_of(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

#[tokio::test]
async fn a_proxy_fetches_once_and_then_serves_the_cache() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    let reader = {
        world.grant_read("reader", &[&repo]);
        principal("reader")
    };
    world
        .upstream
        .route("/tools/cli-1.0.tgz", vec![Reply::ok("archive bytes")]);

    let first = world.get(&repo, "tools/cli-1.0.tgz", &reader).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(header_of(&first, "x-rustryx-cache"), Some("MISS"));
    assert_eq!(&body_bytes(first).await[..], b"archive bytes");
    world.settle().await;

    let second = world.get(&repo, "tools/cli-1.0.tgz", &reader).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(header_of(&second, "x-rustryx-cache"), Some("HIT"));
    assert_eq!(
        header_of(&second, "x-checksum-sha256").map(str::len),
        Some(64)
    );
    assert_eq!(&body_bytes(second).await[..], b"archive bytes");
    assert_eq!(world.upstream.calls_for("/tools/cli-1.0.tgz"), 1);

    // Ranges and conditional requests are answered from the cached copy.
    let part = world
        .get_with(
            &repo,
            "tools/cli-1.0.tgz",
            &reader,
            &[("range", "bytes=0-6")],
        )
        .await;
    assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&body_bytes(part).await[..], b"archive");
    assert_eq!(world.upstream.calls_for("/tools/cli-1.0.tgz"), 1);
}

#[tokio::test]
async fn a_proxy_takes_no_writes_and_no_deletes() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_all("admin", &[&repo]);
    let admin = principal("admin");
    for method in [Method::PUT, Method::DELETE] {
        let response = world
            .send(&repo, method.clone(), "a.txt", &admin, Body::from("x"))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
        assert_eq!(header_of(&response, "allow"), Some("GET, HEAD"));
    }
    assert_eq!(world.upstream.calls(), 0);
}

#[tokio::test]
async fn a_missing_upstream_file_is_a_404_that_is_remembered() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    assert_eq!(
        world.get(&repo, "nope.txt", &reader).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.get(&repo, "nope.txt", &reader).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.upstream.calls_for("/nope.txt"),
        1,
        "the negative cache answered the second"
    );
}

#[tokio::test]
async fn must_not_forward_an_upstream_401_or_403() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    world
        .upstream
        .route("/locked.txt", vec![Reply::status(403)]);
    world.upstream.route("/auth.txt", vec![Reply::status(401)]);
    for path in ["locked.txt", "auth.txt"] {
        let response = world.get(&repo, path, &reader).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY, "{path}");
        assert!(header_of(&response, "www-authenticate").is_none());
    }
}

#[tokio::test]
async fn must_not_serve_or_cache_content_with_a_wrong_upstream_checksum() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    let wrong = "0".repeat(64);
    world.upstream.route(
        "/bad.bin",
        vec![Reply::ok("tampered").header("x-checksum-sha256", &wrong)],
    );
    let response = world.get(&repo, "bad.bin", &reader).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    world.settle().await;
    let again = world.get(&repo, "bad.bin", &reader).await;
    assert_eq!(again.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        world.upstream.calls_for("/bad.bin"),
        2,
        "nothing was cached"
    );

    // The same file with the announced digest being right is served.
    world.upstream.route(
        "/good.bin",
        vec![Reply::ok("genuine").header("x-checksum-sha256", &sha256_of(b"genuine"))],
    );
    let good = world.get(&repo, "good.bin", &reader).await;
    assert_eq!(good.status(), StatusCode::OK);
    assert_eq!(&body_bytes(good).await[..], b"genuine");
}

#[tokio::test]
async fn must_not_serve_upstream_html_inline() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    world.upstream.route(
        "/site/index.html",
        vec![Reply::ok("<script>alert(1)</script>").header("content-type", "text/html")],
    );
    for expected_cache in ["MISS", "HIT"] {
        let response = world.get(&repo, "site/index.html", &reader).await;
        assert_eq!(
            header_of(&response, "x-rustryx-cache"),
            Some(expected_cache)
        );
        assert!(
            header_of(&response, "content-disposition")
                .is_some_and(|v| v.starts_with("attachment;")),
            "{expected_cache}: {:?}",
            response.headers()
        );
        assert_eq!(
            header_of(&response, "x-content-type-options"),
            Some("nosniff")
        );
        drop(body_bytes(response).await);
        world.settle().await;
    }
}

#[tokio::test]
async fn an_expired_copy_is_revalidated_with_the_upstream_validators() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    world.upstream.route(
        "/v.txt",
        vec![
            Reply::ok("version one").header("etag", "\"v1\""),
            Reply::status(304).header("etag", "\"v1\""),
        ],
    );
    drop(body_bytes(world.get(&repo, "v.txt", &reader).await).await);
    world.settle().await;
    world.clock.advance(Duration::from_secs(2 * 3600));
    let response = world.get(&repo, "v.txt", &reader).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_of(&response, "x-rustryx-cache"), Some("HIT"));
    assert_eq!(&body_bytes(response).await[..], b"version one");
    let requests = world.upstream.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]
            .headers
            .get("if-none-match")
            .and_then(|v| v.to_str().ok()),
        Some("\"v1\"")
    );
}

#[tokio::test]
async fn an_expired_copy_is_served_stale_when_the_upstream_fails() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    world
        .upstream
        .route("/s.txt", vec![Reply::ok("old"), Reply::status(500)]);
    drop(body_bytes(world.get(&repo, "s.txt", &reader).await).await);
    world.settle().await;
    world.clock.advance(Duration::from_secs(2 * 3600));
    let response = world.get(&repo, "s.txt", &reader).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_of(&response, "x-rustryx-cache"), Some("STALE"));
    assert!(header_of(&response, "warning").is_some_and(|w| w.starts_with("110")));
    assert_eq!(&body_bytes(response).await[..], b"old");
}

#[tokio::test]
async fn a_proxy_computes_a_side_file_only_from_a_cached_base() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    world.upstream.route("/pkg.tgz", vec![Reply::ok("bytes")]);

    // Not cached yet: the proxy does not download the base file just to hash it.
    assert_eq!(
        world.get(&repo, "pkg.tgz.sha256", &reader).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(world.upstream.calls_for("/pkg.tgz"), 0);

    drop(body_bytes(world.get(&repo, "pkg.tgz", &reader).await).await);
    world.settle().await;
    let side = world.get(&repo, "pkg.tgz.sha256", &reader).await;
    assert_eq!(side.status(), StatusCode::OK);
    assert_eq!(body_text(side).await, sha256_of(b"bytes"));
}

#[tokio::test]
async fn a_proxy_lists_what_it_has_cached() {
    let world = World::new();
    let repo = world.proxy("mirror").await;
    world.grant_read("reader", &[&repo]);
    let reader = principal("reader");
    world.upstream.route("/dist/a.tgz", vec![Reply::ok("a")]);
    world.upstream.route("/dist/b.tgz", vec![Reply::ok("b")]);
    let before = world.get_with(&repo, "dist/", &reader, &JSON).await;
    assert_eq!(before.status(), StatusCode::NOT_FOUND);
    for path in ["dist/a.tgz", "dist/b.tgz"] {
        drop(body_bytes(world.get(&repo, path, &reader).await).await);
        world.settle().await;
    }
    let listing = world.get_with(&repo, "dist/", &reader, &JSON).await;
    assert_eq!(entry_names(&body_json(listing).await), ["a.tgz", "b.tgz"]);
    // The listing never asks the upstream for a directory.
    assert_eq!(world.upstream.calls_for("/dist/"), 0);
}

#[tokio::test]
async fn a_group_answers_with_the_first_member_that_has_the_path() {
    let world = World::new();
    let first = world.hosted("first").await;
    let second = world.hosted("second").await;
    let group = world.group("all", &[&first, &second]).await;
    world.seed(&first, "both.txt", b"from first").await;
    world.seed(&second, "both.txt", b"from second").await;
    world.seed(&second, "only-second.txt", b"second only").await;
    world.grant_read("reader", &[&group, &first, &second]);
    let reader = principal("reader");

    let both = world.get(&group, "both.txt", &reader).await;
    assert_eq!(
        &body_bytes(both).await[..],
        b"from first",
        "no merge, the first member wins"
    );
    let only = world.get(&group, "only-second.txt", &reader).await;
    assert_eq!(&body_bytes(only).await[..], b"second only");
    assert_eq!(
        world.get(&group, "nowhere.txt", &reader).await.status(),
        StatusCode::NOT_FOUND
    );
    let head = world.head_of(&group, "only-second.txt", &reader).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(header_of(&head, "content-length"), Some("11"));
}

#[tokio::test]
async fn a_group_takes_no_writes() {
    let world = World::new();
    let hosted = world.hosted("files").await;
    let group = world.group("all", &[&hosted]).await;
    world.grant_all("admin", &[&group, &hosted]);
    let admin = principal("admin");
    for (method, path) in [
        (Method::PUT, "a.txt"),
        (Method::DELETE, "a.txt"),
        (Method::PUT, "d/"),
    ] {
        let response = world
            .send(&group, method.clone(), path, &admin, Body::from("x"))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}"
        );
        assert_eq!(header_of(&response, "allow"), Some("GET, HEAD"));
    }
}

#[tokio::test]
async fn must_not_leak_a_members_file_through_a_group_without_the_members_right() {
    let world = World::new();
    let open = world.hosted("open").await;
    let closed = world.hosted("closed").await;
    let group = world.group("all", &[&closed, &open]).await;
    world.seed(&closed, "secret.txt", b"closed data").await;
    world.seed(&open, "secret.txt", b"open data").await;
    world.seed(&closed, "only-closed.txt", b"closed only").await;
    // The caller may read the group and `open`, not `closed`.
    world.grant_read("reader", &[&group, &open]);
    let reader = principal("reader");
    let shadowed = world.get(&group, "secret.txt", &reader).await;
    assert_eq!(&body_bytes(shadowed).await[..], b"open data");
    let hidden = world.get(&group, "only-closed.txt", &reader).await;
    assert_eq!(hidden.status(), StatusCode::NOT_FOUND);
    // A caller who may read nothing behind the group learns nothing from it either.
    world.grant_read("outsider", &[&group]);
    let outsider = principal("outsider");
    assert_eq!(
        world.get(&group, "secret.txt", &outsider).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.get(&group, "secret.txt", &anonymous()).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_group_lists_the_first_member_that_has_the_directory() {
    let world = World::new();
    let first = world.hosted("first").await;
    let second = world.hosted("second").await;
    let group = world.group("all", &[&first, &second]).await;
    world.seed(&first, "shared/from-first.txt", b"1").await;
    world.seed(&second, "shared/from-second.txt", b"2").await;
    world.seed(&second, "second-only/x.txt", b"x").await;
    world.grant_read("reader", &[&group, &first, &second]);
    let reader = principal("reader");
    let shared = world.get_with(&group, "shared/", &reader, &JSON).await;
    assert_eq!(entry_names(&body_json(shared).await), ["from-first.txt"]);
    let only = world.get_with(&group, "second-only/", &reader, &JSON).await;
    assert_eq!(entry_names(&body_json(only).await), ["x.txt"]);
    assert_eq!(
        world
            .get_with(&group, "nothing/", &reader, &JSON)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    // The repository the client addressed is the one named in the listing.
    let listing = world.get_with(&group, "shared/", &reader, &JSON).await;
    assert_eq!(body_json(listing).await["repository"], "all");
}

#[tokio::test]
async fn a_group_of_hosted_and_proxy_prefers_the_hosted_file_and_falls_through() {
    let world = World::new();
    let hosted = world.hosted("local").await;
    let proxy = world.proxy("mirror").await;
    let group = world.group("all", &[&hosted, &proxy]).await;
    world.seed(&hosted, "tools/own.sh", b"own").await;
    world
        .upstream
        .route("/tools/own.sh", vec![Reply::ok("upstream version")]);
    world
        .upstream
        .route("/tools/remote.sh", vec![Reply::ok("remote")]);
    world.grant_read("reader", &[&group, &hosted, &proxy]);
    let reader = principal("reader");

    let own = world.get(&group, "tools/own.sh", &reader).await;
    assert_eq!(&body_bytes(own).await[..], b"own");
    assert_eq!(
        world.upstream.calls_for("/tools/own.sh"),
        0,
        "the hosted member answered"
    );
    let remote = world.get(&group, "tools/remote.sh", &reader).await;
    assert_eq!(&body_bytes(remote).await[..], b"remote");
    world.settle().await;
    assert_eq!(world.upstream.calls_for("/tools/remote.sh"), 1);
}

#[tokio::test]
async fn must_not_serve_html_inline_through_a_group() {
    let world = World::new();
    let hosted = world.hosted("local").await;
    let group = world.group("all", &[&hosted]).await;
    let admin = world.admin(&[&hosted]);
    world
        .put_typed(
            &hosted,
            "page.html",
            &admin,
            b"<script>1</script>",
            Some("text/html"),
        )
        .await;
    world.grant_read("reader", &[&group, &hosted]);
    let response = world.get(&group, "page.html", &principal("reader")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        header_of(&response, "content-disposition").is_some_and(|v| v.starts_with("attachment"))
    );
    assert_eq!(
        header_of(&response, "x-content-type-options"),
        Some("nosniff")
    );
}

#[tokio::test]
async fn a_group_answers_side_files_from_the_member_that_holds_the_base() {
    let world = World::new();
    let hosted = world.hosted("local").await;
    let group = world.group("all", &[&hosted]).await;
    world.seed(&hosted, "pkg.tgz", b"bytes").await;
    world.grant_read("reader", &[&group, &hosted]);
    let side = world
        .get(&group, "pkg.tgz.sha1", &principal("reader"))
        .await;
    assert_eq!(side.status(), StatusCode::OK);
    assert_eq!(body_text(side).await.len(), 40);
}

#[tokio::test]
async fn must_not_resolve_traversal_through_a_proxy_or_a_group() {
    let world = World::new();
    let hosted = world.hosted("local").await;
    let proxy = world.proxy("mirror").await;
    let group = world.group("all", &[&hosted, &proxy]).await;
    world.grant_read("reader", &[&group, &hosted, &proxy]);
    let reader = principal("reader");
    for path in ["../etc/passwd", "a/%2e%2e/b", "..%5c..%5cx", "a//b", "a%00"] {
        for repo in [&proxy, &group] {
            let response = world.get(repo, path, &reader).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        }
    }
    assert_eq!(
        world.upstream.calls(),
        0,
        "no bad path reached the upstream"
    );
}
