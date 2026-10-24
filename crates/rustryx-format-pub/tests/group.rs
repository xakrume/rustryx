//! Contract tests of the group repository: the merge, and the pinning of names against
//! dependency confusion, through `FormatEngine`.
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

use http::{Method, StatusCode};
use rustryx_core::identity::Principal;
use rustryx_core::model::Repository;
use rustryx_format_core::testing::Reply;
use rustryx_format_pub::testing::{package_archive, sha256_hex};
use support::{BASE, Offered, PUB_JSON, World, anonymous, path_below, principal, script_upstream};

struct Setup {
    world: World,
    hosted: Arc<Repository>,
    proxy: Arc<Repository>,
    group: Arc<Repository>,
    alice: Principal,
    dave: Principal,
}

/// hosted first, then the proxy: the order the format recommends. `alice` publishes, `dave`
/// reads the group and the proxy but has no right on the hosted member.
async fn setup() -> Setup {
    let world = World::new();
    let hosted = world.hosted("pub-hosted").await;
    let proxy = world.proxy_at("pub-proxy", "https://pub.example").await;
    let group = world.group("pub-group", &[&hosted, &proxy]).await;
    world.rbac.grant_all("alice", "pub-hosted");
    world.rbac.grant_read("alice", "pub-proxy");
    world.rbac.grant_read("alice", "pub-group");
    world.rbac.grant_read("dave", "pub-proxy");
    world.rbac.grant_read("dave", "pub-group");
    Setup {
        world,
        hosted,
        proxy,
        group,
        alice: principal("alice"),
        dave: principal("dave"),
    }
}

fn versions_of(list: &serde_json::Value) -> Vec<String> {
    list["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn the_group_merges_hosted_and_proxied_versions_of_a_package_that_only_the_proxy_pins() {
    // `fresh` exists upstream only; the hosted member has nothing of it.
    let s = setup().await;
    let a = package_archive("fresh", "1.0.0");
    let b = package_archive("fresh", "1.1.0");
    script_upstream(
        &s.world,
        "fresh",
        &[Offered::new("1.0.0", &a), Offered::new("1.1.0", &b)],
    );
    let list = s.world.list(&s.group, "fresh", &s.alice).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.header("content-type"), Some(PUB_JSON));
    let json = list.json();
    assert_eq!(versions_of(&json), ["1.0.0", "1.1.0"]);
    // Every URL points at the group the client addressed, not at a member.
    for entry in json["versions"].as_array().unwrap() {
        let url = entry["archive_url"].as_str().unwrap();
        assert!(
            url.starts_with(&format!("{BASE}/repository/pub-group/")),
            "{url}"
        );
    }
    // And the download through that URL works and matches the digest in the listing.
    let url = json["versions"][1]["archive_url"].as_str().unwrap();
    let download = s
        .world
        .get(&s.group, &path_below(&s.group, url), &s.alice)
        .await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.body.as_ref(), b.as_slice());
    assert_eq!(json["versions"][1]["archive_sha256"], sha256_hex(&b));
}

#[tokio::test]
async fn the_first_member_wins_a_version_both_have() {
    let s = setup().await;
    let hosted_archive = s
        .world
        .publish_ok(&s.hosted, &s.alice, "both", "1.0.0")
        .await;
    // The proxy offers the same version with different bytes and a higher one.
    // The hosted member pins the name, so the proxy is not even asked: see the next test.
    let theirs = package_archive("both", "1.0.0-upstream");
    script_upstream(&s.world, "both", &[Offered::new("1.0.0", &theirs)]);
    let json = s.world.list(&s.group, "both", &s.alice).await.json();
    assert_eq!(
        json["versions"][0]["archive_sha256"],
        sha256_hex(&hosted_archive)
    );
    let download = s
        .world
        .get(&s.group, "packages/both/versions/1.0.0.tar.gz", &s.alice)
        .await;
    assert_eq!(download.body.as_ref(), hosted_archive.as_slice());
}

#[tokio::test]
async fn must_not_let_a_proxy_member_add_versions_of_a_name_a_hosted_member_has() {
    // The dependency confusion attack: an internal package `acme_billing` is hosted, and the
    // public registry carries an `acme_billing 99.0.0` that somebody else published.
    let s = setup().await;
    let internal = s
        .world
        .publish_ok(&s.hosted, &s.alice, "acme_billing", "1.0.0")
        .await;
    let attacker = package_archive("acme_billing", "99.0.0");
    script_upstream(
        &s.world,
        "acme_billing",
        &[Offered::new("99.0.0", &attacker)],
    );
    script_upstream(
        &s.world,
        "other_pkg",
        &[Offered::new(
            "1.0.0",
            &package_archive("other_pkg", "1.0.0"),
        )],
    );

    let json = s
        .world
        .list(&s.group, "acme_billing", &s.alice)
        .await
        .json();
    assert_eq!(
        versions_of(&json),
        ["1.0.0"],
        "the public version must not appear"
    );
    assert_eq!(json["latest"]["version"], "1.0.0");

    // A direct download of the public archive through the group is not possible either, and
    // neither is the single version document.
    s.world
        .get(
            &s.group,
            "packages/acme_billing/versions/99.0.0.tar.gz",
            &s.alice,
        )
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
    s.world
        .get(
            &s.group,
            "api/packages/acme_billing/versions/99.0.0",
            &s.alice,
        )
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
    s.world.settle().await;
    assert_eq!(
        s.world.upstream.calls_for("/api/packages/acme_billing"),
        0,
        "the upstream was asked about a pinned name"
    );
    assert_eq!(
        s.world
            .upstream
            .calls_for("/packages/acme_billing/versions/99.0.0.tar.gz"),
        0
    );

    // The internal version still downloads, and an unrelated package still comes from the proxy.
    let download = s
        .world
        .get(
            &s.group,
            "packages/acme_billing/versions/1.0.0.tar.gz",
            &s.alice,
        )
        .await;
    assert_eq!(download.body.as_ref(), internal.as_slice());
    let other = s.world.list(&s.group, "other_pkg", &s.alice).await;
    assert_eq!(versions_of(&other.json()), ["1.0.0"]);
}

#[tokio::test]
async fn the_claim_holds_even_for_a_caller_who_may_not_read_the_hosted_member() {
    let s = setup().await;
    s.world
        .publish_ok(&s.hosted, &s.alice, "acme_billing", "1.0.0")
        .await;
    script_upstream(
        &s.world,
        "acme_billing",
        &[Offered::new(
            "99.0.0",
            &package_archive("acme_billing", "99.0.0"),
        )],
    );
    // dave cannot read the hosted member. For him the internal package does not exist, and the
    // public look-alike must not take its place.
    let list = s.world.list(&s.group, "acme_billing", &s.dave).await;
    list.assert_pub_error(StatusCode::NOT_FOUND);
    s.world
        .get(
            &s.group,
            "packages/acme_billing/versions/99.0.0.tar.gz",
            &s.dave,
        )
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
    assert_eq!(s.world.upstream.calls_for("/api/packages/acme_billing"), 0);
}

#[tokio::test]
async fn a_hosted_member_listed_after_a_proxy_does_not_pin_against_it() {
    // Only hosted members BEFORE a proxy claim against it. This is the documented order rule,
    // and the reason hosted members go first.
    let world = World::new();
    let hosted = world.hosted("pub-hosted").await;
    let proxy = world.proxy_at("pub-proxy", "https://pub.example").await;
    let group = world.group("pub-group-reversed", &[&proxy, &hosted]).await;
    world.rbac.grant_all("alice", "pub-hosted");
    world.rbac.grant_read("alice", "pub-proxy");
    world.rbac.grant_read("alice", "pub-group-reversed");
    let alice = principal("alice");
    world
        .publish_ok(&hosted, &alice, "acme_billing", "1.0.0")
        .await;
    script_upstream(
        &world,
        "acme_billing",
        &[Offered::new(
            "99.0.0",
            &package_archive("acme_billing", "99.0.0"),
        )],
    );
    let json = world.list(&group, "acme_billing", &alice).await.json();
    assert_eq!(versions_of(&json), ["1.0.0", "99.0.0"]);
}

/// Two proxies with different upstream prefixes, as a group.
async fn two_proxies() -> (World, Arc<Repository>, Principal) {
    let world = World::new();
    let first = world.proxy_at("pub-first", "https://pub.example/a").await;
    let second = world.proxy_at("pub-second", "https://pub.example/b").await;
    let group = world.group("pub-two", &[&first, &second]).await;
    for name in ["pub-first", "pub-second", "pub-two"] {
        world.rbac.grant_read("erin", name);
    }
    (world, group, principal("erin"))
}

fn discontinued(package: &str, offered: &[Offered<'_>], by: &str) -> Vec<u8> {
    let mut doc: serde_json::Value =
        serde_json::from_slice(&support::upstream_listing(package, offered)).unwrap();
    doc["isDiscontinued"] = true.into();
    doc["replacedBy"] = by.into();
    serde_json::to_vec(&doc).unwrap()
}

#[tokio::test]
async fn package_level_fields_come_from_the_first_member_when_nobody_holds_a_claim() {
    let (world, group, erin) = two_proxies().await;
    let one = package_archive("demo", "1.0.0");
    let two = package_archive("demo", "2.0.0");
    // The second member says the package is discontinued, the first does not: the first decides.
    support::script_upstream_at(&world, "/a", "demo", &[Offered::new("1.0.0", &one)]);
    world.upstream.route(
        "/b/api/packages/demo",
        vec![Reply::ok(discontinued(
            "demo",
            &[Offered::new("2.0.0", &two)],
            "successor",
        ))],
    );
    let json = world.list(&group, "demo", &erin).await.json();
    assert_eq!(versions_of(&json), ["1.0.0", "2.0.0"]);
    assert!(json.get("isDiscontinued").is_none(), "{json}");

    // The other way round: the first member's discontinuation is what the client sees.
    let (world, group, erin) = two_proxies().await;
    world.upstream.route(
        "/a/api/packages/demo",
        vec![Reply::ok(discontinued(
            "demo",
            &[Offered::new("1.0.0", &one)],
            "successor",
        ))],
    );
    support::script_upstream_at(&world, "/b", "demo", &[Offered::new("2.0.0", &two)]);
    let json = world.list(&group, "demo", &erin).await.json();
    assert_eq!(json["isDiscontinued"], true);
    assert_eq!(json["replacedBy"], "successor");
}

#[tokio::test]
async fn retracted_versions_stay_retracted_through_the_group() {
    let s = setup().await;
    let a = package_archive("demo", "1.0.0");
    let b = package_archive("demo", "2.0.0");
    script_upstream(
        &s.world,
        "demo",
        &[
            Offered::new("1.0.0", &a),
            Offered::new("2.0.0", &b).retracted(),
        ],
    );
    let json = s.world.list(&s.group, "demo", &s.alice).await.json();
    assert_eq!(json["versions"][1]["retracted"], true);
    assert_eq!(json["latest"]["version"], "1.0.0");
}

#[tokio::test]
async fn the_single_version_document_comes_from_the_first_member_that_has_it() {
    let s = setup().await;
    let internal = s
        .world
        .publish_ok(&s.hosted, &s.alice, "mine", "1.0.0")
        .await;
    let single = s
        .world
        .get(&s.group, "api/packages/mine/versions/1.0.0", &s.alice)
        .await;
    assert_eq!(single.status, StatusCode::OK);
    assert_eq!(single.header("content-type"), Some(PUB_JSON));
    let json = single.json();
    assert_eq!(json["archive_sha256"], sha256_hex(&internal));
    assert!(
        json["archive_url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{BASE}/repository/pub-group/"))
    );
}

#[tokio::test]
async fn must_not_leak_data_through_the_group_without_rights_on_the_member() {
    let s = setup().await;
    let internal = s
        .world
        .publish_ok(&s.hosted, &s.alice, "secret_tool", "1.0.0")
        .await;
    // dave may read the group but not the hosted member that has the package.
    s.world
        .list(&s.group, "secret_tool", &s.dave)
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
    s.world
        .get(
            &s.group,
            "packages/secret_tool/versions/1.0.0.tar.gz",
            &s.dave,
        )
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
    s.world
        .get(&s.group, "api/packages/secret_tool/versions/1.0.0", &s.dave)
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
    // alice, who may read it, gets the bytes.
    let download = s
        .world
        .get(
            &s.group,
            "packages/secret_tool/versions/1.0.0.tar.gz",
            &s.alice,
        )
        .await;
    assert_eq!(download.body.as_ref(), internal.as_slice());
}

#[tokio::test]
async fn must_not_answer_401_for_a_failed_member_lookup_and_never_forward_a_member_error() {
    let s = setup().await;
    s.world
        .publish_ok(&s.hosted, &s.alice, "mine", "1.0.0")
        .await;
    // A failing authorization lookup for the hosted member fails closed: the answer is 403 with
    // the pub challenge (the caller has a valid token), never 401 and never "not found".
    s.world.rbac.fail_repository("pub-hosted");
    let answer = s.world.list(&s.group, "mine", &s.alice).await;
    answer.assert_pub_error(StatusCode::FORBIDDEN);

    // A proxy member whose upstream answers 401 or 403 is a failed member, not a refusal that
    // reaches the client.
    for code in [401u16, 403] {
        let world = World::new();
        let proxy = world.proxy_at("pub-proxy", "https://pub.example").await;
        let group = world.group("pub-group", &[&proxy]).await;
        world.rbac.grant_read("frank", "pub-proxy");
        world.rbac.grant_read("frank", "pub-group");
        world
            .upstream
            .route("/api/packages/demo", vec![Reply::status(code)]);
        let answer = world.list(&group, "demo", &principal("frank")).await;
        answer.assert_pub_error(StatusCode::BAD_GATEWAY);
        assert_eq!(answer.header("x-rustryx-group-skipped"), None);
    }
}

#[tokio::test]
async fn a_member_that_is_down_makes_the_listing_say_it_may_be_incomplete() {
    let (world, group, erin) = two_proxies().await;
    let two = package_archive("demo", "2.0.0");
    world
        .upstream
        .route("/a/api/packages/demo", vec![Reply::status(503)]);
    support::script_upstream_at(&world, "/b", "demo", &[Offered::new("2.0.0", &two)]);
    let answer = world.list(&group, "demo", &erin).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(versions_of(&answer.json()), ["2.0.0"]);
    // The client is told the answer may lack versions, and nothing may cache it.
    assert_eq!(answer.header("x-rustryx-group-skipped"), Some("1"));
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    // With every member up there is no marker.
    let (world, group, erin) = two_proxies().await;
    support::script_upstream_at(
        &world,
        "/a",
        "demo",
        &[Offered::new("1.0.0", &package_archive("demo", "1.0.0"))],
    );
    support::script_upstream_at(&world, "/b", "demo", &[Offered::new("2.0.0", &two)]);
    let answer = world.list(&group, "demo", &erin).await;
    assert_eq!(answer.header("x-rustryx-group-skipped"), None);
    assert_eq!(versions_of(&answer.json()), ["1.0.0", "2.0.0"]);
}

#[tokio::test]
async fn a_nested_group_resolves_like_a_member() {
    let s = setup().await;
    s.world.rbac.grant_read("alice", "pub-outer");
    let outer = s.world.group("pub-outer", &[&s.group]).await;
    let internal = s
        .world
        .publish_ok(&s.hosted, &s.alice, "mine", "1.0.0")
        .await;
    let json = s.world.list(&outer, "mine", &s.alice).await.json();
    // The URLs name the group the client addressed, the outer one.
    let url = json["versions"][0]["archive_url"].as_str().unwrap();
    assert!(
        url.starts_with(&format!("{BASE}/repository/pub-outer/")),
        "{url}"
    );
    let download = s
        .world
        .get(&outer, &path_below(&outer, url), &s.alice)
        .await;
    assert_eq!(download.body.as_ref(), internal.as_slice());
}

#[tokio::test]
async fn nothing_can_be_published_through_a_group_and_advisories_do_not_exist_there() {
    let s = setup().await;
    s.world.rbac.grant_all("alice", "pub-group");
    for (method, path) in [
        (Method::GET, "api/packages/versions/new"),
        (
            Method::POST,
            "api/packages/versions/upload/0199c9a4-7a58-7d3e-8c1a-5f6d2b9e4a10",
        ),
    ] {
        let answer = s
            .world
            .send(
                &s.group,
                method,
                path,
                &s.alice,
                axum_core::body::Body::empty(),
            )
            .await;
        answer.assert_pub_error(StatusCode::METHOD_NOT_ALLOWED);
    }
    s.world
        .get(&s.group, "api/packages/mine/advisories", &s.alice)
        .await
        .assert_pub_error(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_anonymous_caller_of_a_group_is_told_to_authenticate() {
    let s = setup().await;
    s.world
        .list(&s.group, "demo", &anonymous())
        .await
        .assert_pub_error(StatusCode::UNAUTHORIZED);
    // A caller with a token but no right on the group is told 403.
    s.world
        .list(&s.group, "demo", &principal("nobody"))
        .await
        .assert_pub_error(StatusCode::FORBIDDEN);
    let _ = &s.proxy;
}
