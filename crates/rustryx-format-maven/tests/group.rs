//! Group repositories: merged metadata, derived checksums of merged documents, dependency
//! confusion protection, member rights and failures.
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

mod support;

use std::sync::Arc;

use axum_core::body::Body;
use http::{Method, header};
use rustryx_core::model::Repository;
use rustryx_format_core::testing::Reply;
use rustryx_format_maven::metadata::parse_metadata;
use support::*;

const METADATA: &str = "org/acme/lib/maven-metadata.xml";
const JAR_1_0: &str = "org/acme/lib/1.0/lib-1.0.jar";
const SNAPSHOT_DIR: &str = "org/acme/lib/1.0-SNAPSHOT";

fn up(path: &str) -> String {
    format!("/maven2/{path}")
}

fn metadata_xml(versions: &[&str], last_updated: &str) -> String {
    let list: String = versions
        .iter()
        .map(|v| format!("<version>{v}</version>"))
        .collect();
    format!(
        "<metadata><groupId>org.acme</groupId><artifactId>lib</artifactId><versioning>\
         <latest>0.0</latest><release>0.0</release><versions>{list}</versions>\
         <lastUpdated>{last_updated}</lastUpdated></versioning></metadata>"
    )
}

async fn versions_of(
    w: &World,
    group: &Arc<Repository>,
    who: &rustryx_core::identity::Principal,
) -> Vec<String> {
    let response = w.get(group, METADATA, who).await;
    assert_eq!(status(&response), 200);
    let text = body_text(response).await;
    parse_metadata(text.as_bytes())
        .unwrap()
        .versioning
        .unwrap()
        .versions
}

fn read_all(w: &World, repositories: &[&Arc<Repository>]) -> rustryx_core::identity::Principal {
    for repository in repositories {
        w.rbac.grant_read("dev", repository.name.as_str());
    }
    principal("dev")
}

#[tokio::test]
async fn metadata_of_two_hosted_members_is_the_union_with_recomputed_latest_and_release() {
    let w = World::new();
    let a = w.hosted("team-a").await;
    let b = w.hosted("team-b").await;
    let group = w.group("all", &[&a, &b]).await;
    let admin = w.admin(&[&a, &b]);
    w.deploy(&a, &admin, "org/acme", "lib", "1.0").await;
    w.deploy(&a, &admin, "org/acme", "lib", "1.10").await;
    w.deploy(&b, &admin, "org/acme", "lib", "1.9").await;
    w.put(
        &b,
        "org/acme/lib/2.0-SNAPSHOT/lib-2.0-20240102.030405-1.jar",
        &admin,
        "s",
    )
    .await;
    w.put(&b, "org/acme/lib/1.10/lib-1.10.jar", &admin, "duplicate")
        .await;
    let dev = read_all(&w, &[&group, &a, &b]);

    let response = w.get(&group, METADATA, &dev).await;
    assert_eq!(status(&response), 200);
    assert_eq!(content_type(&response), "application/xml");
    let parsed = parse_metadata(body_text(response).await.as_bytes()).unwrap();
    let v = parsed.versioning.unwrap();
    assert_eq!(v.versions, ["1.0", "1.9", "1.10", "2.0-SNAPSHOT"]);
    assert_eq!(v.latest.as_deref(), Some("2.0-SNAPSHOT"));
    assert_eq!(v.release.as_deref(), Some("1.10"));
    assert!(v.last_updated.is_some());
}

#[tokio::test]
async fn metadata_of_two_proxies_is_the_union_and_ignores_the_values_members_claim() {
    let w = World::new();
    let one = w.proxy("central-one").await;
    let two = w.proxy("central-two").await;
    // Both point at the same scripted upstream host, so give them different paths through the
    // metadata they return: the fake routes by path, so use one document that both read.
    let group = w.group("all", &[&one, &two]).await;
    w.upstream.route(
        &up(METADATA),
        vec![Reply::ok(metadata_xml(&["1.0", "1.1"], "20240101000000"))],
    );
    let dev = read_all(&w, &[&group, &one, &two]);
    let v = versions_of(&w, &group, &dev).await;
    assert_eq!(v, ["1.0", "1.1"]);
    let text = body_text(w.get(&group, METADATA, &dev).await).await;
    let parsed = parse_metadata(text.as_bytes()).unwrap().versioning.unwrap();
    assert_eq!(
        parsed.latest.as_deref(),
        Some("1.1"),
        "not the 0.0 the member wrote"
    );
    assert_eq!(parsed.release.as_deref(), Some("1.1"));
}

#[tokio::test]
async fn must_not_let_a_proxy_member_add_versions_of_a_name_a_hosted_member_has() {
    let w = World::new();
    let internal = w.hosted("internal").await;
    let central = w.proxy("central").await;
    let group = w.group("all", &[&internal, &central]).await;
    let admin = w.admin(&[&internal]);
    w.deploy(&internal, &admin, "org/acme", "lib", "1.0").await;
    let dev = read_all(&w, &[&group, &internal, &central]);

    // The attacker's copy on the public repository.
    w.upstream.route(
        &up(METADATA),
        vec![Reply::ok(metadata_xml(&["1.0", "99.0"], "20240101000000"))],
    );
    w.upstream.route(
        &up("org/acme/lib/99.0/lib-99.0.jar"),
        vec![Reply::ok("evil")],
    );
    w.upstream.route(
        &up("org/acme/lib/99.0/lib-99.0.jar.sha1"),
        vec![Reply::ok(sha1_hex(b"evil"))],
    );

    assert_eq!(
        versions_of(&w, &group, &dev).await,
        ["1.0"],
        "the version list has no 99.0"
    );
    let jar = w.get(&group, "org/acme/lib/99.0/lib-99.0.jar", &dev).await;
    assert_eq!(status(&jar), 404);
    assert_eq!(
        w.upstream.calls(),
        0,
        "the public repository was never asked about the name"
    );
    let sha = w
        .get(&group, "org/acme/lib/99.0/lib-99.0.jar.sha1", &dev)
        .await;
    assert_eq!(status(&sha), 404);
    let sums = w.get(&group, &format!("{METADATA}.sha1"), &dev).await;
    assert_eq!(status(&sums), 200);
    assert_eq!(w.upstream.calls(), 0);
}

#[tokio::test]
async fn the_claim_holds_for_a_caller_who_cannot_read_the_claiming_member() {
    let w = World::new();
    let internal = w.hosted("internal").await;
    let central = w.proxy("central").await;
    let group = w.group("all", &[&internal, &central]).await;
    let admin = w.admin(&[&internal]);
    w.deploy(&internal, &admin, "org/acme", "lib", "1.0").await;
    w.upstream.route(
        &up(METADATA),
        vec![Reply::ok(metadata_xml(&["99.0"], "20240101000000"))],
    );
    // Rights on the group and the proxy only: the hosted member is invisible, the public copy
    // must not take its place.
    let outsider = read_all(&w, &[&group, &central]);
    assert_eq!(status(&w.get(&group, METADATA, &outsider).await), 404);
    assert_eq!(w.upstream.calls(), 0);
}

#[tokio::test]
async fn a_name_nobody_hosts_comes_from_the_proxy() {
    let w = World::new();
    let internal = w.hosted("internal").await;
    let central = w.proxy("central").await;
    let group = w.group("all", &[&internal, &central]).await;
    let admin = w.admin(&[&internal]);
    w.deploy(&internal, &admin, "org/acme", "lib", "1.0").await;
    let dev = read_all(&w, &[&group, &internal, &central]);
    let other = "org/acme/other/maven-metadata.xml";
    let xml = "<metadata><groupId>org.acme</groupId><artifactId>other</artifactId>\
               <versioning><versions><version>3.0</version></versions></versioning></metadata>";
    w.upstream.route(&up(other), vec![Reply::ok(xml)]);
    let response = w.get(&group, other, &dev).await;
    assert_eq!(status(&response), 200);
    let parsed = parse_metadata(body_text(response).await.as_bytes()).unwrap();
    assert_eq!(parsed.versioning.unwrap().versions, ["3.0"]);
}

#[tokio::test]
async fn a_checksum_of_merged_metadata_is_the_digest_of_the_merged_document() {
    let w = World::new();
    let a = w.hosted("team-a").await;
    let b = w.hosted("team-b").await;
    let group = w.group("all", &[&a, &b]).await;
    let admin = w.admin(&[&a, &b]);
    w.deploy(&a, &admin, "org/acme", "lib", "1.0").await;
    w.deploy(&b, &admin, "org/acme", "lib", "1.1").await;
    let dev = read_all(&w, &[&group, &a, &b]);
    let xml = body_text(w.get(&group, METADATA, &dev).await).await;
    for (suffix, expected) in [
        ("sha1", sha1_hex(xml.as_bytes())),
        ("md5", md5_hex(xml.as_bytes())),
        ("sha256", sha256_hex(xml.as_bytes())),
        ("sha512", sha512_hex(xml.as_bytes())),
    ] {
        let response = w.get(&group, &format!("{METADATA}.{suffix}"), &dev).await;
        assert_eq!(status(&response), 200, "{suffix}");
        assert_eq!(body_text(response).await, expected, "{suffix}");
    }
    // Neither member's own digest describes the merged document.
    let member_digest = body_text(w.get(&a, &format!("{METADATA}.sha1"), &admin).await).await;
    assert_ne!(member_digest, sha1_hex(xml.as_bytes()));
}

#[tokio::test]
async fn an_incomplete_merge_is_marked_and_its_checksum_inherits_the_marker() {
    let w = World::new();
    let flaky = w.proxy("flaky").await;
    let hosted = w.hosted("team-a").await;
    // The failing proxy comes first, so the claim of the hosted member (which only blocks proxies
    // AFTER it) does not hide it: it is asked, it fails, and the merge is incomplete.
    let group = w.group("all", &[&flaky, &hosted]).await;
    let admin = w.admin(&[&hosted]);
    w.deploy(&hosted, &admin, "org/acme", "lib", "1.0").await;
    w.upstream.route(&up(METADATA), vec![Reply::status(503)]);
    let dev = read_all(&w, &[&group, &hosted, &flaky]);

    let response = w.get(&group, METADATA, &dev).await;
    assert_eq!(status(&response), 200);
    assert_eq!(
        header_of(response.headers(), "x-rustryx-group-skipped"),
        Some("1")
    );
    assert_eq!(
        header_of(response.headers(), "cache-control"),
        Some("no-store")
    );
    let xml = body_text(response).await;

    let sum = w.get(&group, &format!("{METADATA}.sha1"), &dev).await;
    assert_eq!(status(&sum), 200);
    assert_eq!(
        header_of(sum.headers(), "x-rustryx-group-skipped"),
        Some("1")
    );
    assert_eq!(header_of(sum.headers(), "cache-control"), Some("no-store"));
    assert_eq!(body_text(sum).await, sha1_hex(xml.as_bytes()));
}

#[tokio::test]
async fn when_nobody_has_it_and_a_member_failed_the_group_answers_with_the_failure() {
    let w = World::new();
    let hosted = w.hosted("team-a").await;
    let flaky = w.proxy("flaky").await;
    let group = w.group("all", &[&hosted, &flaky]).await;
    let admin = w.admin(&[&hosted]);
    // The hosted member has a different artifact than the proxy is asked about, so no claim applies.
    w.deploy(&hosted, &admin, "org/acme", "unrelated", "1.0")
        .await;
    w.upstream.route(&up(METADATA), vec![Reply::status(503)]);
    let dev = read_all(&w, &[&group, &hosted, &flaky]);
    let response = w.get(&group, METADATA, &dev).await;
    assert_eq!(
        status(&response),
        502,
        "not a 404: a member that may have it was down"
    );
}

#[tokio::test]
async fn must_not_leak_data_through_a_group_without_member_rights() {
    let w = World::new();
    let secret = w.hosted("secret").await;
    let open = w.hosted("open").await;
    let group = w.group("all", &[&secret, &open]).await;
    let admin = w.admin(&[&secret, &open]);
    w.deploy(&secret, &admin, "org/acme", "lib", "1.0").await;
    w.deploy(&open, &admin, "org/acme", "lib", "1.1").await;
    // The user may read the group and `open`, not `secret`.
    let dev = read_all(&w, &[&group, &open]);
    assert_eq!(versions_of(&w, &group, &dev).await, ["1.1"]);
    assert_eq!(status(&w.get(&group, JAR_1_0, &dev).await), 404);
    assert_eq!(
        status(&w.get(&group, "org/acme/lib/1.1/lib-1.1.jar", &dev).await),
        200
    );
}

#[tokio::test]
async fn a_group_serves_a_file_from_the_first_member_that_has_it() {
    let w = World::new();
    let a = w.hosted("team-a").await;
    let b = w.hosted("team-b").await;
    let group = w.group("all", &[&a, &b]).await;
    let admin = w.admin(&[&a, &b]);
    w.put(&b, JAR_1_0, &admin, "from b").await;
    let dev = read_all(&w, &[&group, &a, &b]);
    assert_eq!(
        body_text(w.get(&group, JAR_1_0, &dev).await).await,
        "from b"
    );
    w.put(&a, JAR_1_0, &admin, "from a").await;
    assert_eq!(
        body_text(w.get(&group, JAR_1_0, &dev).await).await,
        "from a"
    );
    let sha = w.get(&group, &format!("{JAR_1_0}.sha1"), &dev).await;
    assert_eq!(body_text(sha).await, sha1_hex(b"from a"));
}

#[tokio::test]
async fn must_not_accept_writes_to_a_group() {
    let w = World::new();
    let a = w.hosted("team-a").await;
    let group = w.group("all", &[&a]).await;
    let admin = w.admin(&[&a, &group]);
    for method in [Method::PUT, Method::DELETE] {
        let response = w
            .send(&group, method, JAR_1_0, &admin, Body::from("x"))
            .await;
        assert_eq!(status(&response), 405);
    }
}

#[tokio::test]
async fn a_member_that_serves_garbage_is_skipped_and_cannot_take_the_group_down() {
    let w = World::new();
    let hosted = w.hosted("team-a").await;
    let proxy = w.proxy("central").await;
    let group = w.group("all", &[&hosted, &proxy]).await;
    let admin = w.admin(&[&hosted]);
    // The hosted member has the name for `lib`; use another name for the proxy-only document.
    w.deploy(&hosted, &admin, "org/acme", "other", "1.0").await;
    let dev = read_all(&w, &[&group, &hosted, &proxy]);
    let xxe = "<?xml version=\"1.0\"?><!DOCTYPE metadata [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]>\
        <metadata><groupId>org.acme</groupId><artifactId>&xxe;</artifactId></metadata>";
    w.upstream
        .route(&up("org/acme/lib/maven-metadata.xml"), vec![Reply::ok(xxe)]);
    // Only a garbage document answers: nothing can be merged.
    let response = w.get(&group, METADATA, &dev).await;
    assert_eq!(status(&response), 422);
    assert!(!body_text(response).await.contains("root:"));
}

#[tokio::test]
async fn garbage_from_one_member_does_not_hide_the_good_document_of_another() {
    let w = World::new();
    let one = w.proxy("central-one").await;
    let hosted = w.hosted("team-a").await;
    let group = w.group("all", &[&one, &hosted]).await;
    let admin = w.admin(&[&hosted]);
    w.deploy(&hosted, &admin, "org/acme", "lib", "2.0").await;
    w.upstream.route(
        &up(METADATA),
        vec![Reply::ok("<html>502 bad gateway</html>")],
    );
    let dev = read_all(&w, &[&group, &one, &hosted]);
    // The proxy comes first and serves garbage; the hosted member holds the name, but the claim
    // only blocks proxies that come AFTER a hosted member, so both answer.
    assert_eq!(versions_of(&w, &group, &dev).await, ["2.0"]);
}

#[tokio::test]
async fn a_path_outside_the_layout_is_not_served_by_a_group() {
    let w = World::new();
    let proxy = w.proxy("central").await;
    let group = w.group("all", &[&proxy]).await;
    w.upstream.route(
        &up("archetype-catalog.xml"),
        vec![Reply::ok("<archetype-catalog/>")],
    );
    let dev = read_all(&w, &[&group, &proxy]);
    assert_eq!(
        status(&w.get(&group, "archetype-catalog.xml", &dev).await),
        404
    );
    assert_eq!(w.upstream.calls(), 0);
}

#[tokio::test]
async fn merged_metadata_supports_etag_and_head() {
    let w = World::new();
    let a = w.hosted("team-a").await;
    let b = w.hosted("team-b").await;
    let group = w.group("all", &[&a, &b]).await;
    let admin = w.admin(&[&a, &b]);
    w.deploy(&a, &admin, "org/acme", "lib", "1.0").await;
    w.deploy(&b, &admin, "org/acme", "lib", "1.1").await;
    let dev = read_all(&w, &[&group, &a, &b]);
    let first = w.get(&group, METADATA, &dev).await;
    let etag = header_of(first.headers(), "etag").unwrap().to_owned();
    let mut conditional = w.head(Method::GET, METADATA, &dev);
    conditional
        .headers
        .insert(header::IF_NONE_MATCH, header_value(&etag));
    assert_eq!(
        status(&w.send_with(&group, conditional, Body::empty()).await),
        304
    );
    let head = w.head_request(&group, METADATA, &dev).await;
    assert_eq!(status(&head), 200);
    assert_eq!(
        header_of(head.headers(), "content-length").unwrap(),
        body_text(first).await.len().to_string()
    );
    // The checksum is cacheable by its own validators.
    let sums = w.get(&group, &format!("{METADATA}.sha1"), &dev).await;
    let sum_etag = header_of(sums.headers(), "etag").unwrap().to_owned();
    let mut conditional = w.head(Method::GET, &format!("{METADATA}.sha1"), &dev);
    conditional
        .headers
        .insert(header::IF_NONE_MATCH, header_value(&sum_etag));
    assert_eq!(
        status(&w.send_with(&group, conditional, Body::empty()).await),
        304
    );
}

// ---- snapshots ------------------------------------------------------------------------

async fn deploy_snapshot(
    w: &World,
    repo: &Arc<Repository>,
    who: &rustryx_core::identity::Principal,
    stamp: &str,
    extensions: &[&str],
) {
    for ext in extensions {
        let path = format!("{SNAPSHOT_DIR}/lib-1.0-{stamp}.{ext}");
        assert_eq!(
            status(&w.put(repo, &path, who, format!("{ext} {stamp}")).await),
            201
        );
    }
}

#[tokio::test]
async fn the_member_with_the_newest_snapshot_build_wins_as_a_whole() {
    let w = World::new();
    let a = w.hosted("team-a").await;
    let b = w.hosted("team-b").await;
    let group = w.group("all", &[&a, &b]).await;
    let admin = w.admin(&[&a, &b]);
    deploy_snapshot(
        &w,
        &a,
        &admin,
        "20240102.030405-3",
        &["jar", "pom", "module"],
    )
    .await;
    deploy_snapshot(&w, &b, &admin, "20240103.030405-1", &["jar"]).await;
    let dev = read_all(&w, &[&group, &a, &b]);
    let response = w
        .get(&group, &format!("{SNAPSHOT_DIR}/maven-metadata.xml"), &dev)
        .await;
    assert_eq!(status(&response), 200);
    let v = parse_metadata(body_text(response).await.as_bytes())
        .unwrap()
        .versioning
        .unwrap();
    assert_eq!(
        v.snapshot.unwrap().timestamp.as_deref(),
        Some("20240103.030405")
    );
    assert_eq!(
        v.snapshot_versions.len(),
        1,
        "entries of two members are not mixed"
    );
    // The build the winner names is served by the group.
    let jar = w
        .get(
            &group,
            &format!("{SNAPSHOT_DIR}/lib-1.0-20240103.030405-1.jar"),
            &dev,
        )
        .await;
    assert_eq!(body_text(jar).await, "jar 20240103.030405-1");
}

#[tokio::test]
async fn plugin_prefix_metadata_of_two_proxies_is_merged_by_prefix() {
    let w = World::new();
    let one = w.proxy("central-one").await;
    let group = w.group("all", &[&one]).await;
    let path = "org/apache/maven/plugins/maven-metadata.xml";
    let xml = "<metadata><plugins><plugin><name>Clean</name><prefix>clean</prefix>\
        <artifactId>maven-clean-plugin</artifactId></plugin><plugin><name>Clean again</name>\
        <prefix>clean</prefix><artifactId>maven-clean-plugin</artifactId></plugin></plugins></metadata>";
    w.upstream.route(&up(path), vec![Reply::ok(xml)]);
    let dev = read_all(&w, &[&group, &one]);
    let response = w.get(&group, path, &dev).await;
    assert_eq!(status(&response), 200);
    let parsed = parse_metadata(body_text(response).await.as_bytes()).unwrap();
    assert_eq!(parsed.group_id.as_deref(), Some("org.apache.maven.plugins"));
    assert_eq!(parsed.plugins.len(), 1);
    assert_eq!(parsed.plugins[0].name.as_deref(), Some("Clean"));
}
